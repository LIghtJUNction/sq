use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Question {
    Choice {
        instructions: String,
        criteria: BTreeMap<String, Option<String>>,
    },
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
    Noul {
        instructions: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<BTreeMap<String, String>>,
    },
}

impl Question {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Choice { .. } => "choice",
            Self::Score { .. } => "score",
            Self::Noul { .. } => "noul",
        }
    }

    pub fn validate(&self) -> Result<()> {
        let instructions = match self {
            Self::Choice {
                instructions,
                criteria,
            } => {
                ensure!(
                    (2..=255).contains(&criteria.len()),
                    "choice needs 2..255 distinct options"
                );
                ensure!(
                    criteria.keys().all(|k| !k.trim().is_empty()),
                    "choice labels must not be blank"
                );
                instructions
            }
            Self::Score {
                instructions,
                criteria,
            } => {
                ensure!(
                    (2..=10).contains(&criteria.len()),
                    "score needs 2..10 ordered levels"
                );
                ensure!(
                    criteria.iter().all(|x| !x.trim().is_empty()),
                    "score levels must not be blank"
                );
                instructions
            }
            Self::Noul {
                instructions,
                criteria,
            } => {
                if let Some(c) = criteria {
                    ensure!(
                        c.len() == 2 && c.contains_key("true") && c.contains_key("false"),
                        "noul criteria must contain true and false"
                    );
                    ensure!(
                        c.values().all(|x| !x.trim().is_empty()),
                        "noul descriptions must not be blank"
                    );
                }
                instructions
            }
        };
        ensure!(
            !instructions.trim().is_empty(),
            "instructions must not be blank"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub state: Value,
    pub questions: BTreeMap<String, Question>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl Request {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=512).contains(&self.questions.len()),
            "request needs 1..512 questions"
        );
        ensure!(
            self.model.as_ref().is_none_or(|m| !m.trim().is_empty()),
            "model must not be blank"
        );
        for (id, q) in &self.questions {
            ensure!(
                !id.trim().is_empty() && id.len() <= 128,
                "question ids must contain 1..128 bytes"
            );
            q.validate()?;
        }
        Ok(())
    }
}

fn number(v: &Value, key: &str, min: f64, max: f64) -> Result<f64> {
    let n = v.get(key).and_then(Value::as_f64);
    ensure!(
        n.is_some_and(|n| n.is_finite() && n >= min && n <= max),
        "invalid or missing {key}"
    );
    Ok(n.unwrap())
}

pub fn validate_response(request: &Request, response: &Value) -> Result<()> {
    let answers = response
        .get("answers")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("response is missing answers"))?;
    ensure!(
        answers.len() == request.questions.len(),
        "answer count differs from question count"
    );
    for (id, question) in &request.questions {
        let a = answers
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("missing answer {id}"))?;
        ensure!(
            a.get("type").and_then(Value::as_str) == Some(question.kind()),
            "answer {id} has wrong type"
        );
        let keys: Vec<String> = match question {
            Question::Noul { .. } => {
                number(a, "noul", 0.0, 1.0)?;
                continue;
            }
            Question::Choice { criteria, .. } => {
                let choice = a.get("choice").and_then(Value::as_str).unwrap_or("");
                ensure!(
                    criteria.contains_key(choice),
                    "answer {id} selected an undeclared choice"
                );
                criteria.keys().cloned().collect()
            }
            Question::Score { criteria, .. } => {
                number(a, "score", 0.0, (criteria.len() - 1) as f64)?;
                let legend = a
                    .get("legend")
                    .ok_or_else(|| anyhow::anyhow!("missing score legend"))?;
                for (i, text) in criteria.iter().enumerate() {
                    let entry = if legend.is_array() {
                        legend.get(i)
                    } else {
                        legend.get(i.to_string())
                    };
                    ensure!(
                        entry.and_then(Value::as_str) == Some(text),
                        "score legend differs from requested levels"
                    );
                }
                ensure!(
                    legend
                        .as_array()
                        .map(Vec::len)
                        .or_else(|| legend.as_object().map(|m| m.len()))
                        == Some(criteria.len()),
                    "invalid legend size"
                );
                (0..criteria.len()).map(|i| i.to_string()).collect()
            }
        };
        number(a, "confidence", 0.0, 1.0)?;
        let probabilities = a
            .get("probabilities")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("missing probabilities"))?;
        ensure!(
            probabilities.len() == keys.len(),
            "probability labels differ from criteria"
        );
        let mut sum = 0.0;
        for key in keys {
            sum += number(&a["probabilities"], &key, 0.0, 1.0)?;
        }
        ensure!((sum - 1.0).abs() <= 0.01, "probabilities do not sum to one");
    }
    Ok(())
}

pub fn passes(answer: &Value, confidence: Option<f64>, threshold: Option<f64>) -> Result<bool> {
    if let Some(gate) = confidence {
        let c = answer
            .get("confidence")
            .and_then(Value::as_f64)
            .ok_or_else(|| {
                anyhow::anyhow!("this answer has no confidence; Noul uses --threshold instead")
            })?;
        if c < gate {
            return Ok(false);
        }
    }
    if let Some(t) = threshold {
        let n = answer
            .get("score")
            .or_else(|| answer.get("noul"))
            .and_then(Value::as_f64);
        match n {
            Some(n) if n >= t => {}
            Some(_) => return Ok(false),
            None => bail!("--threshold requires score or noul"),
        }
    }
    Ok(true)
}
