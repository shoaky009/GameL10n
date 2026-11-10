//! Repair work is bound to host observations, never to a model's claim of success.
use super::*;
use crate::VerificationIssue;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub(crate) enum Status {
    Open,
    Analyzing,
    Patched,
    Verifying,
    Passed,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Scope {
    Discovery,
    Trial(Vec<String>),
    Full,
}

impl Scope {
    pub fn from_args(args: &Value) -> Self {
        if args["discoveryOnly"] == true {
            Self::Discovery
        } else if let Some(files) = args["trialFiles"].as_array() {
            Self::Trial(
                files
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|file| file.replace('\\', "/").to_lowercase())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            )
        } else {
            Self::Full
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Attempt {
    implementation: String,
    evidence: String,
    passed: bool,
    approach: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Ticket {
    pub id: String,
    pub status: Status,
    pub issue: VerificationIssue,
    pub scope: Scope,
    pub input_fingerprint: String,
    pub attempts: Vec<Attempt>,
    pub observations: u32,
    pub approach: Option<String>,
    pub last_analysis: Option<String>,
    pub evidence: String,
    pub evidence_file: Option<String>,
}

/// Uses registered input identities; only the verifier can decide whether those bytes are current.
pub(super) fn input_key(state: &Session) -> String {
    let hashes = state
        .extra
        .get("resourceHashes")
        .and_then(Value::as_object)
        .map(|hashes| hashes.iter().collect::<BTreeMap<_, _>>());
    hash_text(&json!({"resources":hashes,"binding":state.extra.get("inputBinding"),"objective":state.objective}).to_string())
}

pub(super) fn begin(state: &mut Session, scope: &Scope) {
    let input = input_key(state);
    for ticket in &mut state.repairs {
        if ticket.input_fingerprint == input
            && ticket.status != Status::Passed
            && (scope == &Scope::Full || &ticket.scope == scope)
        {
            ticket.status = Status::Verifying;
        }
    }
}

pub(super) fn observe(state: &mut Session, scope: Scope, implementation: &str, result: &Value) {
    let input = input_key(state);
    let passed = result["passed"] == true;
    let issues: Vec<VerificationIssue> =
        serde_json::from_value(result["issues"].clone()).unwrap_or_default();
    let evidence = hash_text(&json!({"input":input,"implementation":implementation,"scope":scope,"passed":passed,"issues":issues,"diagnostics":result["diagnostics"]}).to_string());
    if passed {
        for ticket in &mut state.repairs {
            if ticket.input_fingerprint == input
                && ticket.status != Status::Passed
                && (scope == Scope::Full || ticket.scope == scope)
            {
                ticket.status = Status::Passed;
                attempt(ticket, implementation, &evidence, true);
            }
        }
        return;
    }
    for ticket in &mut state.repairs {
        if ticket.input_fingerprint == input && ticket.status == Status::Verifying {
            ticket.status = Status::Failed;
        }
    }
    for issue in issues {
        let id = hash_text(&json!({"input":input,"scope":scope,"code":issue.code,"stage":issue.stage,"resource":issue.resource_path,"unit":issue.unit_id}).to_string())[..24].to_owned();
        if let Some(ticket) = state.repairs.iter_mut().find(|ticket| ticket.id == id) {
            ticket.issue = issue;
            ticket.evidence_file = result["evidenceFile"].as_str().map(str::to_owned);
            ticket.observations = ticket.observations.saturating_add(1);
            ticket.status = Status::Failed;
            attempt(ticket, implementation, &evidence, false);
        } else {
            let mut ticket = Ticket {
                id,
                status: Status::Open,
                issue,
                scope: scope.clone(),
                input_fingerprint: input.clone(),
                attempts: Vec::new(),
                observations: 1,
                approach: None,
                last_analysis: None,
                evidence: evidence.clone(),
                evidence_file: result["evidenceFile"].as_str().map(str::to_owned),
            };
            attempt(&mut ticket, implementation, &evidence, false);
            state.repairs.push(ticket);
        }
    }
    // Keep recent investigations; complete tool results remain in the session archive.
    if state.repairs.len() > 64 {
        let index = state
            .repairs
            .iter()
            .position(|ticket| ticket.status == Status::Passed || ticket.input_fingerprint != input)
            .unwrap_or(0);
        state.repairs.remove(index);
    }
}

fn attempt(ticket: &mut Ticket, implementation: &str, evidence: &str, passed: bool) {
    ticket.evidence = evidence.into();
    if ticket.attempts.last().is_some_and(|previous| {
        previous.evidence == evidence && previous.approach == ticket.approach
    }) {
        return;
    }
    ticket.attempts.push(Attempt {
        implementation: implementation.into(),
        evidence: evidence.into(),
        passed,
        approach: ticket.approach.clone(),
    });
    if ticket.attempts.len() > 8 {
        ticket.attempts.remove(0);
    }
}

pub(super) fn patched(state: &mut Session) {
    let input = input_key(state);
    for ticket in &mut state.repairs {
        if ticket.input_fingerprint == input && ticket.status != Status::Passed {
            ticket.status = Status::Patched;
        }
    }
}

pub(super) fn approach(
    state: &Session,
    args: &Value,
) -> Result<Option<(usize, String)>, EngineError> {
    let Some(repair) = args.get("repair") else {
        return Ok(None);
    };
    let id = text(repair, "ticketId")?;
    let approach = text(repair, "approach")?.trim();
    if approach.is_empty() || approach.chars().count() > 600 {
        return Err(EngineError::Protocol("修复方案需要 1～600 字符。".into()));
    }
    let input = input_key(state);
    let index = state
        .repairs
        .iter()
        .position(|ticket| {
            ticket.id == id && ticket.input_fingerprint == input && ticket.status != Status::Passed
        })
        .ok_or_else(|| {
            EngineError::Rejected("修复任务不存在、输入已变化或已经通过验收。".into())
        })?;
    Ok(Some((index, approach.into())))
}

pub(super) fn packet(state: &Session) -> Value {
    let input = input_key(state);
    json!(state.repairs.iter().rev().filter(|ticket| ticket.input_fingerprint == input && ticket.status != Status::Passed).take(8)
        .map(|ticket| json!({"ticket":ticket,"noNewVerificationEvidence":ticket.last_analysis.as_ref()==Some(&ticket.evidence),"constraint":"Preserve IDs, unchanged data and protected tokens. Trial success is not full acceptance; runtime remains untested.","expectedResult":"Existing host verification passes for the required scope."})).collect::<Vec<_>>())
}

pub(super) fn analyzing(state: &mut Session) {
    let input = input_key(state);
    for ticket in &mut state.repairs {
        if ticket.input_fingerprint == input && ticket.status != Status::Passed {
            ticket.last_analysis = Some(ticket.evidence.clone());
            if matches!(ticket.status, Status::Open | Status::Failed) {
                ticket.status = Status::Analyzing;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn failure() -> Value {
        json!({"passed":false,"issues":[{"code":"text-mismatch","stage":"partial-translation-re-extraction","resourcePath":"text.json","unitId":"7","expected":"中文","actual":"原文"}],"diagnostics":["counterexample"]})
    }

    #[test]
    fn repeated_failures_keep_identity_attempts_and_survive_restart() {
        let mut state = Session::default();
        observe(&mut state, Scope::Full, "writer-1", &failure());
        let id = state.repairs[0].id.clone();
        analyzing(&mut state);
        observe(&mut state, Scope::Full, "writer-1", &failure());
        assert_eq!(state.repairs.len(), 1);
        assert_eq!(state.repairs[0].attempts.len(), 1);
        assert_eq!(packet(&state)[0]["noNewVerificationEvidence"], true);
        let update = approach(
            &state,
            &json!({"repair":{"ticketId":id,"approach":"Fix the confirmed writer offset"}}),
        )
        .unwrap()
        .unwrap();
        state.repairs[update.0].approach = Some(update.1);
        patched(&mut state);
        begin(&mut state, &Scope::Full);
        assert_eq!(state.repairs[0].status, Status::Verifying);
        let mut restored: Session =
            serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
        observe(
            &mut restored,
            Scope::Full,
            "writer-2",
            &json!({"passed":true}),
        );
        assert_eq!(restored.repairs[0].status, Status::Passed);
        assert_eq!(restored.repairs[0].attempts.len(), 2);
    }

    #[test]
    fn trial_or_changed_inputs_cannot_resolve_a_full_failure() {
        let mut state = Session::default();
        observe(&mut state, Scope::Full, "writer", &failure());
        observe(
            &mut state,
            Scope::Trial(vec!["text.json".into()]),
            "writer",
            &json!({"passed":true}),
        );
        assert_eq!(state.repairs[0].status, Status::Open);
        state
            .extra
            .insert("resourceHashes".into(), json!({"text.json":"new bytes"}));
        assert_eq!(packet(&state), json!([]));
        observe(&mut state, Scope::Full, "writer", &failure());
        assert_eq!(state.repairs.len(), 2);
        observe(&mut state, Scope::Full, "writer", &json!({"passed":true}));
        assert_eq!(state.repairs[0].status, Status::Open);
        assert_eq!(state.repairs[1].status, Status::Passed);
    }
}
