use serde_json::json;
use sq_system_one::{
    config::{Config, origin},
    input::{self, Format},
    protocol::{Request, passes, validate_response},
};

#[test]
fn origins_and_configs_reject_ambiguous_settings() {
    for url in [
        "https://example.com/v1",
        "https://a:b@example.com",
        "https://example.com?x=1",
        "ftp://example.com",
        "http://example.com/#x",
    ] {
        assert!(origin(url).is_err(), "{url}");
    }
    assert_eq!(
        origin("https://example.com/").unwrap(),
        "https://example.com"
    );
    assert_eq!(
        Config::default().resolve("drex").unwrap().base_url,
        "https://console.nace.ai"
    );
    let c: Config = toml::from_str(include_str!("../config/config.toml")).unwrap();
    c.validate().unwrap();
    assert!(toml::from_str::<Config>("concurency = 8").is_err());
    let temp = tempfile::tempdir().unwrap();
    assert!(Config::load(Some(&temp.path().join("missing.toml"))).is_err());
}

#[test]
fn validate_native_responses_and_confidence_semantics() {
    let request: Request = serde_json::from_value(json!({
        "state": "report", "questions": {
            "team": {"type":"choice","instructions":"Who?","criteria":{"a":"A","b":"B"}},
            "severity": {"type":"score","instructions":"Severity?","criteria":["Low","High"]},
            "urgent": {"type":"noul","instructions":"Urgent?"}
        }
    }))
    .unwrap();
    request.validate().unwrap();
    let mut response = json!({"answers": {
        "team": {"type":"choice","choice":"a","probabilities":{"a":0.9,"b":0.1},"confidence":0.8},
        "severity": {"type":"score","score":0.8,"probabilities":{"0":0.2,"1":0.8},"legend":{"0":"Low","1":"High"},"confidence":0.7},
        "urgent": {"type":"noul","noul":0.95}
    }});
    validate_response(&request, &response).unwrap();
    response["answers"]["severity"]["legend"] = json!(["Low", "High"]);
    validate_response(&request, &response).unwrap();
    assert!(passes(&response["answers"]["urgent"], None, Some(0.9)).unwrap());
    assert!(passes(&response["answers"]["urgent"], Some(0.9), None).is_err());
    assert!(!passes(&response["answers"]["team"], Some(0.9), None).unwrap());
    response["answers"]["team"]["choice"] = json!("invented");
    assert!(validate_response(&request, &response).is_err());
    response["answers"]["team"]["choice"] = json!("a");
    response["answers"]["severity"]["score"] = json!(100);
    assert!(validate_response(&request, &response).is_err());
}

fn ingest(text: &str, format: Format, limit: usize) -> Vec<anyhow::Result<serde_json::Value>> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    input::produce(std::io::Cursor::new(text), format, limit, tx);
    let mut out = vec![];
    while let Ok(item) = rx.try_recv() {
        out.push(item);
    }
    out
}

#[test]
fn inputs_stream_json_text_and_quoted_csv() {
    let values = ingest("{\"id\":1}\nhello\n", Format::Auto, 1024);
    assert_eq!(values[0].as_ref().unwrap(), &json!({"id":1}));
    assert_eq!(values[1].as_ref().unwrap(), &json!("hello"));
    let csv = "id,message\r\n1,\"hello, \"\"world\"\"\"\r\n2,\"line 1\nline 2\"\r\n";
    let rows = ingest(csv, Format::Csv, 1024);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].as_ref().unwrap(),
        &json!({"id":"1","message":"hello, \"world\""})
    );
    assert_eq!(
        rows[1].as_ref().unwrap(),
        &json!({"id":"2","message":"line 1\nline 2"})
    );
    assert!(ingest("a,a\n1,2\n", Format::Csv, 1024)[0].is_err());
    assert!(ingest("a,b\n1,2,3\n", Format::Csv, 1024)[0].is_err());
    assert!(ingest("abcdefghijk\n", Format::Text, 4)[0].is_err());
    assert!(ingest("a,b\n1,\"abcdefghijklmn\"\n", Format::Csv, 8)[0].is_err());
    assert!(ingest("bad\n", Format::Jsonl, 1024)[0].is_err());
    let rows = ingest("[1,2]", Format::Json, 1024);
    assert_eq!(rows.len(), 2);
}
