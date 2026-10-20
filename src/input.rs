use anyhow::{Context, Result, ensure};
use clap::ValueEnum;
use serde_json::Value;
use std::io::{BufRead, Read};
use tokio::sync::mpsc;

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum Format {
    #[default]
    Auto,
    Text,
    Jsonl,
    Json,
    Csv,
}

fn line<R: BufRead>(reader: &mut R, limit: usize) -> Result<Option<String>> {
    let mut buf = Vec::new();
    let n = reader.take(limit as u64 + 1).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    ensure!(
        n <= limit,
        "input record exceeds --max-record-bytes {limit}"
    );
    if buf.last() == Some(&b'\n') {
        buf.pop();
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    Ok(Some(String::from_utf8(buf).context("input is not UTF-8")?))
}

pub fn produce<R: BufRead>(
    mut reader: R,
    format: Format,
    limit: usize,
    sender: mpsc::Sender<Result<Value>>,
) {
    let result = (|| -> Result<()> {
        match format {
            Format::Json => {
                let mut bytes = vec![];
                reader.take(limit as u64 + 1).read_to_end(&mut bytes)?;
                ensure!(
                    bytes.len() <= limit,
                    "JSON document exceeds --max-record-bytes {limit}; use JSONL for large streams"
                );
                let value: Value =
                    serde_json::from_slice(&bytes).context("invalid JSON document")?;
                match value {
                    Value::Array(items) => {
                        for item in items {
                            if sender.blocking_send(Ok(item)).is_err() {
                                return Ok(());
                            }
                        }
                    }
                    item => {
                        let _ = sender.blocking_send(Ok(item));
                    }
                }
            }
            Format::Csv => {
                let mut parser = csv_core::Reader::new();
                let mut output = vec![0; limit + 1];
                let mut ends = vec![0; 65_536];
                let (mut used, mut fields, mut consumed) = (0, 0, 0);
                let mut header: Option<Vec<String>> = None;
                loop {
                    let available = reader.fill_buf()?;
                    let available = &available[..available.len().min(limit + 1 - consumed)];
                    let (status, nin, nout, nend) =
                        parser.read_record(available, &mut output[used..], &mut ends[fields..]);
                    reader.consume(nin);
                    consumed += nin;
                    used += nout;
                    fields += nend;
                    ensure!(
                        consumed <= limit,
                        "CSV record exceeds --max-record-bytes {limit}"
                    );
                    match status {
                        csv_core::ReadRecordResult::End => break,
                        csv_core::ReadRecordResult::Record => {
                            let mut start = 0;
                            let mut row = Vec::with_capacity(fields);
                            for end in &ends[..fields] {
                                row.push(
                                    std::str::from_utf8(&output[start..*end])
                                        .context("CSV is not UTF-8")?
                                        .to_string(),
                                );
                                start = *end;
                            }
                            if let Some(headers) = &header {
                                ensure!(
                                    row.len() == headers.len(),
                                    "CSV row has a different number of fields than its header"
                                );
                                let object = headers
                                    .iter()
                                    .cloned()
                                    .zip(row.into_iter().map(Value::String))
                                    .collect();
                                if sender.blocking_send(Ok(Value::Object(object))).is_err() {
                                    return Ok(());
                                }
                            } else {
                                let mut seen = std::collections::BTreeSet::new();
                                ensure!(
                                    row.iter().all(|h| !h.is_empty() && seen.insert(h)),
                                    "CSV headers must be distinct and non-empty"
                                );
                                header = Some(row);
                            }
                            used = 0;
                            fields = 0;
                            consumed = 0;
                        }
                        csv_core::ReadRecordResult::OutputFull => {
                            anyhow::bail!("CSV record exceeds --max-record-bytes {limit}")
                        }
                        csv_core::ReadRecordResult::OutputEndsFull => {
                            anyhow::bail!("CSV record exceeds 65536 fields")
                        }
                        csv_core::ReadRecordResult::InputEmpty => {}
                    }
                }
            }
            Format::Auto | Format::Text | Format::Jsonl => {
                let mut number = 0;
                while let Some(text) = line(&mut reader, limit)? {
                    number += 1;
                    if text.trim().is_empty() {
                        continue;
                    }
                    let item = match format {
                        Format::Text => Value::String(text),
                        Format::Jsonl => match serde_json::from_str(&text)
                            .with_context(|| format!("invalid JSON on line {number}"))
                        {
                            Ok(value) => value,
                            Err(error) => {
                                if sender.blocking_send(Err(error)).is_err() {
                                    return Ok(());
                                }
                                continue;
                            }
                        },
                        _ => serde_json::from_str(&text).unwrap_or(Value::String(text)),
                    };
                    if sender.blocking_send(Ok(item)).is_err() {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = sender.blocking_send(Err(e));
    }
}
