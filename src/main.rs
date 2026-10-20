use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Parser, Subcommand, ValueEnum};
use futures_util::StreamExt;
use serde_json::{Value, json};
use sq_system_one::{
    config::{self, Config, Strategy},
    daemon,
    engine::Engine,
    input::{self, Format},
    protocol::{Question, Request, passes, validate_response},
};
use std::{
    collections::BTreeMap,
    io::{BufReader, Write},
    path::PathBuf,
    sync::Arc,
};

#[derive(Parser)]
#[command(
    version,
    about = "System One decisions for shell pipelines",
    after_help = "Examples:\n  printf '%s\\n' 'disk full' | sq --choice normal,warning,critical -r\n  sq --state-file tickets.json --input-format json --noul 'Is this urgent?'\n  sq daemon\n\nExit status: 0 = output emitted, 1 = no matches, 2 = error.\nInput defaults to one JSON value or text record per line. Use --input-format json for arrays."
)]
struct Cli {
    #[command(flatten)]
    common: Common,
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(long, conflicts_with_all = ["score", "noul", "questions"])]
    choice: Option<String>,
    #[arg(long, conflicts_with_all = ["choice", "noul", "questions"])]
    score: Option<String>,
    #[arg(long, conflicts_with_all = ["choice", "score", "questions"])]
    noul: Option<String>,
    /// JSON or TOML map of named native questions
    #[arg(long)]
    questions: Option<PathBuf>,
    #[arg(long, requires = "choice")]
    instructions: Option<String>,
    /// Ordered score descriptions separated by | (2..10)
    #[arg(long, requires = "score")]
    levels: Option<String>,
    /// Linearly rescale score to 0..N; native score is preserved as score_native
    #[arg(long, requires = "score")]
    score_scale: Option<f64>,
    #[arg(long, alias = "input")]
    state_file: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "auto")]
    input_format: Format,
    #[arg(long, default_value_t = 4 * 1024 * 1024, value_parser = clap::value_parser!(u64).range(1..=67108864))]
    max_record_bytes: u64,
    #[arg(long)]
    confidence_gating: Option<f64>,
    /// Minimum inclusive score (rescaled if requested) or Noul probability
    #[arg(long)]
    threshold: Option<f64>,
    /// Select an answer from a --questions map for gating/extraction
    #[arg(long)]
    question_id: Option<String>,
    /// Dot path or JSON Pointer, e.g. choice, answers.team.choice, /answers/team/choice
    #[arg(long)]
    extract: Option<String>,
    #[arg(short = 'r', long = "raw-output")]
    raw: bool,
    /// Emit matching input records, useful for threshold filters
    #[arg(long, conflicts_with = "extract")]
    emit_input: bool,
    #[arg(long, value_enum, default_value = "json")]
    output_format: OutputFormat,
    #[arg(long, value_enum, default_value = "fail")]
    on_error: OnError,
    /// Send decisions through a running local daemon
    #[arg(long, conflicts_with = "daemon_url")]
    socket: Option<PathBuf>,
    #[arg(long, conflicts_with = "socket")]
    daemon_url: Option<String>,
    #[arg(long, env = "SQ_DAEMON_TOKEN_ENV")]
    daemon_token_env: Option<String>,
}

#[derive(Args)]
struct Common {
    #[arg(long, global = true, env = "SQ_CONFIG")]
    config: Option<PathBuf>,
    #[arg(long, global = true, env = "SQ_PROVIDER")]
    provider: Option<String>,
    #[arg(long, global = true, env = "SQ_BASE_URL")]
    base_url: Option<String>,
    #[arg(long, global = true, env = "SQ_MODEL")]
    model: Option<String>,
    #[arg(long, global = true)]
    api_key_env: Option<String>,
    #[arg(long, global = true, value_delimiter = ',')]
    fallback: Option<Vec<String>>,
    #[arg(long, global = true, value_enum)]
    strategy: Option<Strategy>,
    #[arg(short = 'j', long, global = true)]
    concurrency: Option<usize>,
    #[arg(long, global = true)]
    timeout_secs: Option<u64>,
    #[arg(long, global = true)]
    retries: Option<u32>,
    /// Suppress informational stderr output; errors remain visible
    #[arg(short = 'q', long, global = true)]
    quiet: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Start a bounded local System One relay
    Daemon {
        #[arg(long, conflicts_with = "listen")]
        socket: Option<PathBuf>,
        #[arg(long, conflicts_with = "socket")]
        listen: Option<std::net::SocketAddr>,
        #[arg(long)]
        token_env: Option<String>,
    },
    /// Inspect or create TOML configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Show built-in provider presets
    Providers,
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Write a commented example; refuses to overwrite existing files
    Init,
    /// Show resolved settings (credentials are never printed)
    Show,
    /// Validate configuration without contacting a provider
    Check,
}
#[derive(Clone, Copy, ValueEnum)]
enum OutputFormat {
    Json,
    Jsonl,
    Response,
}
#[derive(Clone, Copy, ValueEnum)]
enum OnError {
    Fail,
    Emit,
}

fn load(common: &Common) -> Result<Config> {
    let mut c = Config::load(common.config.as_deref())?;
    if let Some(p) = &common.provider {
        c.provider = p.clone();
    }
    let p = c.providers.entry(c.provider.clone()).or_default();
    if let Some(v) = &common.base_url {
        p.base_url = Some(v.clone());
    }
    if let Some(v) = &common.model {
        p.model = Some(v.clone());
    }
    if let Some(v) = &common.api_key_env {
        p.api_key_env = Some(v.clone());
    }
    if let Some(v) = &common.fallback {
        c.fallback = v.clone();
    }
    if let Some(v) = common.strategy {
        c.strategy = v;
    }
    if let Some(v) = common.concurrency {
        c.concurrency = v;
    }
    if let Some(v) = common.timeout_secs {
        c.timeout_secs = v;
    }
    if let Some(v) = common.retries {
        c.retries = v;
    }
    c.validate()?;
    Ok(c)
}

fn questions(cli: &Cli) -> Result<BTreeMap<String, Question>> {
    let q = if let Some(path) = &cli.questions {
        let text = std::fs::read_to_string(path).context("cannot read questions file")?;
        if path.extension().is_some_and(|e| e == "toml") {
            toml::from_str(&text)?
        } else {
            serde_json::from_str(&text)?
        }
    } else {
        let question = if let Some(labels) = &cli.choice {
            let labels: Vec<_> = labels.split(',').map(str::trim).collect();
            let criteria: BTreeMap<_, _> = labels
                .iter()
                .map(|x| (x.to_string(), Some(x.to_string())))
                .collect();
            ensure!(
                criteria.len() == labels.len(),
                "choice labels must be distinct"
            );
            Question::Choice {
                instructions: cli
                    .instructions
                    .clone()
                    .unwrap_or_else(|| "Choose the category that best describes the state.".into()),
                criteria,
            }
        } else if let Some(instructions) = &cli.score {
            let criteria = cli
                .levels
                .as_deref()
                .unwrap_or("Absent or negligible|Low|Moderate|High|Extreme or maximal")
                .split('|')
                .map(|s| s.trim().to_string())
                .collect();
            Question::Score {
                instructions: instructions.clone(),
                criteria,
            }
        } else if let Some(instructions) = &cli.noul {
            Question::Noul {
                instructions: instructions.clone(),
                criteria: None,
            }
        } else {
            bail!("provide --choice, --score, --noul, or --questions (see sq --help)");
        };
        BTreeMap::from([("decision".into(), question)])
    };
    let request = Request {
        state: Value::Null,
        questions: q,
        model: None,
    };
    request.validate()?;
    Ok(request.questions)
}

fn extract<'a>(value: &'a Value, path: &str) -> Result<&'a Value> {
    let found = if path.starts_with('/') {
        value.pointer(path)
    } else {
        path.trim_start_matches('.')
            .split('.')
            .filter(|s| !s.is_empty())
            .try_fold(value, |v, k| v.get(k))
    };
    found.ok_or_else(|| anyhow::anyhow!("extraction path {path} is missing"))
}

#[derive(Clone)]
enum Evaluator {
    Direct(Arc<Engine>),
    Relay {
        client: reqwest::Client,
        endpoint: String,
    },
}
impl Evaluator {
    async fn evaluate(&self, request: Request) -> Result<Value> {
        match self {
            Self::Direct(engine) => engine.evaluate(request).await,
            Self::Relay { client, endpoint } => {
                let response = client
                    .post(endpoint)
                    .json(&request)
                    .send()
                    .await
                    .map_err(|_| anyhow::anyhow!("daemon connection failed"))?;
                ensure!(
                    response.status().is_success(),
                    "daemon returned HTTP {}",
                    response.status().as_u16()
                );
                let mut bytes = vec![];
                let mut stream = response.bytes_stream();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.context("daemon response transfer failed")?;
                    ensure!(
                        bytes.len() + chunk.len() <= 8 * 1024 * 1024,
                        "daemon response exceeds 8 MiB"
                    );
                    bytes.extend_from_slice(&chunk);
                }
                let response =
                    serde_json::from_slice(&bytes).context("daemon returned invalid JSON")?;
                validate_response(&request, &response)?;
                Ok(response)
            }
        }
    }
}

fn evaluator(cli: &Cli, config: &Config) -> Result<Evaluator> {
    if cli.socket.is_none() && cli.daemon_url.is_none() {
        return Ok(Evaluator::Direct(Arc::new(Engine::new(config)?)));
    }
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(config.timeout_secs));
    if let Some(name) = &cli.daemon_token_env {
        let key = std::env::var(name).with_context(|| format!("missing {name}"))?;
        ensure!(!key.trim().is_empty(), "daemon token is empty");
        let mut headers = reqwest::header::HeaderMap::new();
        let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|_| anyhow::anyhow!("invalid daemon token"))?;
        value.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, value);
        builder = builder.default_headers(headers);
    }
    let endpoint = if let Some(path) = &cli.socket {
        #[cfg(unix)]
        {
            builder = builder.unix_socket(path.clone());
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            bail!("Unix sockets are not supported on this platform");
        }
        "http://localhost/v1/systemone".into()
    } else {
        format!(
            "{}/v1/systemone",
            config::origin(cli.daemon_url.as_deref().unwrap())?
        )
    };
    Ok(Evaluator::Relay {
        client: builder.build()?,
        endpoint,
    })
}

async fn run(cli: Cli) -> Result<u8> {
    if let Some(Command::Config {
        action: ConfigAction::Init,
    }) = &cli.command
    {
        let path = cli
            .common
            .config
            .clone()
            .unwrap_or_else(config::default_path);
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| {
                format!(
                    "cannot create {} (existing files are never overwritten)",
                    path.display()
                )
            })?;
        file.write_all(include_bytes!("../config/config.toml"))?;
        if !cli.common.quiet {
            eprintln!("wrote {}", path.display());
        }
        return Ok(0);
    }
    let mut config = load(&cli.common)?;
    match cli.command {
        Some(Command::Daemon {
            socket,
            listen,
            token_env,
        }) => {
            if socket.is_some() {
                config.daemon.socket = socket;
                config.daemon.listen = None;
            }
            if listen.is_some() {
                config.daemon.listen = listen;
                config.daemon.socket = None;
            }
            if token_env.is_some() {
                config.daemon.token_env = token_env;
            }
            daemon::serve(config, cli.common.quiet).await?;
            return Ok(0);
        }
        Some(Command::Config { action }) => {
            match action {
                ConfigAction::Show => println!("{}", toml::to_string_pretty(&config)?),
                ConfigAction::Check => {
                    if !cli.common.quiet {
                        eprintln!("configuration valid");
                    }
                }
                ConfigAction::Init => unreachable!(),
            }
            return Ok(0);
        }
        Some(Command::Providers) => {
            for name in ["jev", "drex", "laya"] {
                let p = Config::default().resolve(name)?;
                println!(
                    "{}",
                    json!({"name":p.name,"base_url":p.base_url,"model":p.model,"api_key_env":p.api_key_env})
                );
            }
            return Ok(0);
        }
        None => {}
    }
    let questions = questions(&cli)?;
    let id = cli.question_id.clone().or_else(|| {
        if questions.len() == 1 {
            questions.keys().next().cloned()
        } else {
            None
        }
    });
    if let Some(id) = &id {
        ensure!(questions.contains_key(id), "unknown question id {id}");
    }
    if cli.confidence_gating.is_some()
        || cli.threshold.is_some()
        || (cli.raw && cli.extract.is_none() && !cli.emit_input)
    {
        ensure!(
            id.is_some(),
            "multiple questions require --question-id for gating or implicit raw output"
        );
    }
    if let Some(gate) = cli.confidence_gating {
        ensure!(
            gate.is_finite() && (0.0..=1.0).contains(&gate),
            "confidence gate must be 0..1"
        );
        ensure!(
            !matches!(&questions[id.as_ref().unwrap()], Question::Noul { .. }),
            "Noul has no confidence; use --threshold for its probability"
        );
    }
    if let Some(scale) = cli.score_scale {
        ensure!(
            scale.is_finite() && scale > 0.0,
            "score scale must be positive and finite"
        );
        ensure!(
            !matches!(cli.output_format, OutputFormat::Response),
            "--score-scale changes the score representation; use json/jsonl output instead of native response output"
        );
    }
    ensure!(
        !(cli.raw
            && matches!(cli.output_format, OutputFormat::Response)
            && cli.extract.is_none()
            && !cli.emit_input),
        "-r with --output-format response requires --extract"
    );
    if let Some(threshold) = cli.threshold {
        let q = &questions[id.as_ref().unwrap()];
        let max = match q {
            Question::Noul { .. } => 1.0,
            Question::Score { criteria, .. } => {
                cli.score_scale.unwrap_or((criteria.len() - 1) as f64)
            }
            _ => bail!("--threshold requires score or noul"),
        };
        ensure!(
            threshold.is_finite() && (0.0..=max).contains(&threshold),
            "threshold must be 0..{max}"
        );
    }
    let evaluator = evaluator(&cli, &config)?;
    let (tx, mut rx) = tokio::sync::mpsc::channel(config.concurrency);
    let format = match (
        cli.input_format,
        cli.state_file
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str()),
    ) {
        (Format::Auto, Some("json")) => Format::Json,
        (Format::Auto, Some("jsonl" | "ndjson")) => Format::Jsonl,
        (Format::Auto, Some("csv")) => Format::Csv,
        (format, _) => format,
    };
    let limit = cli.max_record_bytes as usize;
    let input: Box<dyn std::io::BufRead + Send> = match &cli.state_file {
        Some(path) if path.as_os_str() != "-" => Box::new(BufReader::new(
            std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?,
        )),
        _ => Box::new(BufReader::new(std::io::stdin())),
    };
    // A regular producer thread can block on stdin without preventing Tokio shutdown
    // after a downstream broken pipe or early network failure.
    std::thread::spawn(move || input::produce(input, format, limit, tx));
    let source = async_stream::stream! { while let Some(item) = rx.recv().await { yield item; } };
    let jobs = source
        .enumerate()
        .map(|(index, item)| {
            let evaluator = evaluator.clone();
            let questions = questions.clone();
            async move {
                let state = item?;
                let response = evaluator
                    .evaluate(Request {
                        state: state.clone(),
                        questions,
                        model: None,
                    })
                    .await?;
                Ok::<_, anyhow::Error>((index, state, response))
            }
        })
        .buffered(config.concurrency);
    tokio::pin!(jobs);
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    let mut emitted = 0;
    let mut errors = false;
    while let Some(result) = jobs.next().await {
        let (index, state, mut response) = match result {
            Ok(v) => v,
            Err(e) => match cli.on_error {
                OnError::Fail => return Err(e),
                OnError::Emit => {
                    writeln!(
                        output,
                        "{}",
                        json!({"error": {"message": format!("{e:#}")}})
                    )?;
                    output.flush()?;
                    errors = true;
                    continue;
                }
            },
        };
        if let Some(scale) = cli.score_scale {
            let answer = &mut response["answers"][id.as_ref().unwrap()];
            let native = answer["score"].as_f64().context("score missing")?;
            let levels = match &questions[id.as_ref().unwrap()] {
                Question::Score { criteria, .. } => criteria.len(),
                _ => unreachable!(),
            };
            answer["score_native"] = json!(native);
            answer["score"] = json!(native / (levels - 1) as f64 * scale);
        }
        if let Some(id) = &id {
            if !passes(
                &response["answers"][id],
                cli.confidence_gating,
                cli.threshold,
            )? {
                continue;
            }
        }
        let mut row = if matches!(cli.output_format, OutputFormat::Response) {
            response.clone()
        } else if let Some(id) = &id {
            response["answers"][id].clone()
        } else {
            json!({"answers": response["answers"]})
        };
        if !matches!(cli.output_format, OutputFormat::Response) {
            row["input"] = state.clone();
            row["index"] = json!(index);
            row["provider"] = response["_sq"]["provider"].clone();
            if let Some(model) = response.get("model") {
                row["model"] = model.clone();
            }
            if let Some(usage) = response.get("usage") {
                row["usage"] = usage.clone();
            }
        }
        let value = if cli.emit_input {
            &state
        } else if let Some(path) = &cli.extract {
            extract(&row, path)?
        } else if cli.raw {
            let q = &questions[id.as_ref().unwrap()];
            extract(&row, q.kind())?
        } else {
            &row
        };
        if cli.raw && value.is_string() {
            writeln!(output, "{}", value.as_str().unwrap())?;
        } else {
            writeln!(output, "{value}")?;
        }
        output.flush()?;
        emitted += 1;
    }
    Ok(if errors {
        2
    } else if emitted > 0 {
        0
    } else {
        1
    })
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = if matches!(cli.command, Some(Command::Daemon { .. })) {
        run(cli).await
    } else {
        tokio::select! { result = run(cli) => result, _ = daemon::shutdown() => Ok(130) }
    };
    let code = match result {
        Ok(code) => code,
        Err(e)
            if e.chain().any(|e| {
                e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe)
            }) =>
        {
            0
        }
        Err(e) => {
            eprintln!("sq: {e:#}");
            2
        }
    };
    std::process::exit(code.into());
}
