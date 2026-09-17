//! Runtime provider/model availability checks.
//!
//! Static OCG validation proves that configured model keys and variants are
//! internally coherent. This module performs the separate runtime check: the
//! selected OpenCode executable must currently expose every configured Lead
//! and consumer provider/model ID. It uses OpenCode's supported `models` CLI
//! output and never reads provider credential stores.

use crate::error::Result;
use crate::model::{self, ModelRequirement, ModelRequirementKind};
use crate::process::ProcessHost;
use crate::proxy::ChildProxyEnv;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Available,
    MissingProvider,
    MissingModel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCheck {
    pub requirement: ModelRequirement,
    pub availability: Availability,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelPreflight {
    Complete { checks: Vec<ModelCheck> },
    Unavailable { reason: String },
}

impl ModelPreflight {
    /// A missing active Lead is fatal because OpenCode would otherwise be free
    /// to run the OCG Lead identity through an unrelated fallback model.
    pub fn active_lead_failure(&self, active_level: &str) -> Option<String> {
        let agent = model::lead_agent_id(active_level);
        let Self::Complete { checks } = self else {
            return None;
        };
        let check = checks.iter().find(|check| {
            check.requirement.kind == ModelRequirementKind::Lead && check.requirement.agent == agent
        })?;
        match check.availability {
            Availability::Available => None,
            Availability::MissingProvider | Availability::MissingModel => Some(format!(
                "required Lead model {} is not currently exposed by OpenCode; authenticate/configure the provider or adjust OCG configuration",
                check.requirement.full_model_id
            )),
        }
    }

    pub fn missing_non_active_count(&self, active_level: &str) -> usize {
        let active = model::lead_agent_id(active_level);
        match self {
            Self::Complete { checks } => checks
                .iter()
                .filter(|check| {
                    check.availability != Availability::Available
                        && check.requirement.agent != active
                })
                .count(),
            Self::Unavailable { .. } => 0,
        }
    }
}

/// Probe one resolved OpenCode executable with the generated config. Callers
/// remove OCG's not-yet-materialized local plugin while preserving user config.
/// A process failure is a non-fatal `Unavailable` result; callers decide
/// whether to warn (launch) or display an informational diagnostic (doctor).
pub fn probe(
    data: &Value,
    process: &dyn ProcessHost,
    program: &Path,
    cwd: &Path,
    config_content: &str,
    proxy: &ChildProxyEnv,
) -> Result<ModelPreflight> {
    let requirements = model::runtime_model_requirements(data)?;
    let output = match process.models(program, cwd, config_content, proxy) {
        Ok(output) => output,
        Err(_) => {
            return Ok(ModelPreflight::Unavailable {
                reason: "runtime model check could not be completed (`opencode models` failed)"
                    .to_string(),
            })
        }
    };
    Ok(check_output(requirements, &output))
}

pub fn check_output(requirements: Vec<ModelRequirement>, output: &str) -> ModelPreflight {
    let available: BTreeSet<String> = model_tokens(output);
    let providers: BTreeSet<&str> = available
        .iter()
        .filter_map(|model| model.split_once('/').map(|(provider, _)| provider))
        .collect();
    let checks = requirements
        .into_iter()
        .map(|requirement| {
            let availability = if available.contains(&requirement.full_model_id) {
                Availability::Available
            } else {
                let provider = requirement
                    .full_model_id
                    .split_once('/')
                    .map(|(provider, _)| provider)
                    .unwrap_or("");
                if providers.contains(provider) {
                    Availability::MissingModel
                } else {
                    Availability::MissingProvider
                }
            };
            ModelCheck {
                requirement,
                availability,
            }
        })
        .collect();
    ModelPreflight::Complete { checks }
}

/// Every `provider/model` token in the catalogue output.
///
/// `opencode models` prints exactly `provider/model`, one per line. Matching on
/// the token instead of the whole line keeps the check strict (a requirement is
/// available only when that exact id is present) while tolerating decoration a
/// future runtime or terminal wrapper might add: ANSI colours, table borders,
/// trailing punctuation or a trailing description column. A parsed token can
/// only ever make a required id *available*, never silently satisfy a different
/// one, so this cannot produce a false "missing".
fn model_tokens(output: &str) -> BTreeSet<String> {
    strip_ansi(output)
        .split_whitespace()
        .map(|raw| raw.trim_matches(|c: char| !is_model_char(c)))
        .filter(|token| is_model_id(token))
        .map(str::to_string)
        .collect()
}

fn is_model_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_')
}

fn is_model_id(token: &str) -> bool {
    match token.split_once('/') {
        Some((provider, model)) => {
            !provider.is_empty() && !model.is_empty() && !model.contains('/')
        }
        None => false,
    }
}

/// Remove ANSI escape sequences (CSI `ESC [ ... final-byte`) so a coloured
/// catalogue still yields clean tokens. No other interpretation is attempted.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(current) = chars.next() {
        if current != '\u{1b}' {
            out.push(current);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults::{load_defaults, GearSource};
    use crate::process::FakeProcessHost;
    use crate::proxy::{resolve, MapProxyEnv, NoStaticProxy};
    use std::path::PathBuf;

    fn data() -> Value {
        load_defaults(&GearSource::Embedded).unwrap()
    }

    fn all_models() -> &'static str {
        "openai/gpt-5.6-sol\nopenai/gpt-6-astra\nvolcengine-coding/kimi-k2.7-code\nvolcengine-coding/kimi-k3\nopencode-go/deepseek-v4.1-flash\nopencode-go/glm-5.3-flash\nopencode-go/glm-5.3\n"
    }

    #[test]
    fn all_required_models_available_and_duplicate_leads_remain_visible() {
        let requirements = model::runtime_model_requirements(&data()).unwrap();
        let sol = requirements
            .iter()
            .filter(|item| item.full_model_id == "openai/gpt-5.6-sol")
            .collect::<Vec<_>>();
        assert_eq!(sol.len(), 2, "low and mid must be checked independently");
        assert_ne!(sol[0].variant, sol[1].variant);

        let report = check_output(requirements, all_models());
        let ModelPreflight::Complete { checks } = report else {
            panic!("expected complete report")
        };
        assert!(checks
            .iter()
            .all(|check| check.availability == Availability::Available));
    }

    #[test]
    fn decorated_catalogue_output_still_resolves_every_required_model() {
        // ANSI colours, a table border, a trailing description column and
        // trailing punctuation must not hide a present model.
        let decorated = "\u{1b}[1mprovider/model\u{1b}[0m          description\n\
             \u{1b}[32mopenai/gpt-5.6-sol\u{1b}[0m         Sol medium\n\
             | openai/gpt-6-astra      | premium lead |\n\
             * volcengine-coding/kimi-k2.7-code (code plans)\n\
             volcengine-coding/kimi-k3\n\
             opencode-go/deepseek-v4.1-flash,\n\
             opencode-go/glm-5.3-flash\n\
             opencode-go/glm-5.3\n";
        let requirements = model::runtime_model_requirements(&data()).unwrap();
        let report = check_output(requirements, decorated);
        let ModelPreflight::Complete { checks } = report else {
            panic!("expected a complete report")
        };
        assert!(
            checks
                .iter()
                .all(|check| check.availability == Availability::Available),
            "decorated output must not hide a present model: {checks:?}"
        );
    }

    #[test]
    fn documented_urls_and_paths_are_not_treated_as_models() {
        let tokens = model_tokens("see https://opencode.ai/docs/models and /usr/local/bin\n");
        assert!(tokens.is_empty(), "{tokens:?}");
        // A decorated line still yields the exact model token.
        let tokens = model_tokens("  | openai/gpt-5.6-sol |\n");
        assert_eq!(tokens, BTreeSet::from(["openai/gpt-5.6-sol".to_string()]));
    }

    #[test]
    fn description_only_output_reports_a_missing_provider() {
        let requirements = model::runtime_model_requirements(&data()).unwrap();
        // No catalogue entries at all: every requirement must be missing, never
        // silently "available".
        let report = check_output(requirements, "no models are configured\n");
        let ModelPreflight::Complete { checks } = report else {
            panic!("expected a complete report")
        };
        assert!(checks
            .iter()
            .all(|check| check.availability == Availability::MissingProvider));
        assert!(checks
            .iter()
            .any(|check| check.requirement.kind == ModelRequirementKind::Lead));
    }

    #[test]
    fn distinguishes_missing_provider_from_missing_model() {
        let requirements = model::runtime_model_requirements(&data()).unwrap();
        let report = check_output(
            requirements,
            "openai/gpt-5.6-sol\nopencode-go/some-other-model\n",
        );
        let ModelPreflight::Complete { checks } = report else {
            panic!("expected complete report")
        };
        let astra = checks
            .iter()
            .find(|check| check.requirement.label == "lead-high")
            .unwrap();
        assert_eq!(astra.availability, Availability::MissingModel);
        let explore = checks
            .iter()
            .find(|check| check.requirement.label == "explore")
            .unwrap();
        assert_eq!(explore.availability, Availability::MissingProvider);
    }

    #[test]
    fn failed_probe_is_nonfatal_and_secret_safe() {
        let program = PathBuf::from("/fake/opencode");
        let process = FakeProcessHost::new()
            .with_models_failure(&program, "provider failure containing credential-value");
        let proxy = resolve(false, &MapProxyEnv::new(), &NoStaticProxy).child_env();
        let report = probe(
            &data(),
            &process,
            &program,
            Path::new("/project"),
            "{}",
            &proxy,
        )
        .unwrap();
        let ModelPreflight::Unavailable { reason } = report else {
            panic!("expected unavailable report")
        };
        assert!(reason.contains("could not be completed"));
        assert!(!reason.contains("credential-value"));
    }

    #[test]
    fn missing_active_lead_is_a_contract_failure_but_probe_failure_is_not() {
        let requirements = model::runtime_model_requirements(&data()).unwrap();
        let missing = check_output(requirements, "opencode-go/deepseek-v4.1-flash\n");
        assert!(missing.active_lead_failure("low").is_some());
        let unavailable = ModelPreflight::Unavailable {
            reason: "probe unavailable".to_string(),
        };
        assert!(unavailable.active_lead_failure("low").is_none());
    }
}
