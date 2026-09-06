use crate::native::timing::NativeTiming;
use crate::{state::Store, Error, Result};

fn interval(start: Option<i64>, end: Option<i64>) -> Option<u64> {
    end?.checked_sub(start?)
        .and_then(|ms| u64::try_from(ms).ok())
}

pub(super) fn summary(store: &Store) -> Result<serde_json::Value> {
    let timings = super::artifacts::<NativeTiming>(store, "native-timing-")?;
    let requests = super::artifacts::<super::NativeDispatch>(store, "native-request-")?;
    let mut records = Vec::new();
    for (id, request) in requests {
        let Some(timing) = timings.get(&id) else {
            records.push(serde_json::json!({"dispatch_id":id, "role":request.role,
                "observation":"unknown"}));
            continue;
        };
        if timing.dispatch_id != id || timing.role != request.role {
            return Err(Error::Corrupt(
                "native timing does not match its request".into(),
            ));
        }
        records.push(serde_json::json!({
            "dispatch_id":id, "role":request.role, "reuse_requested":request.reuse_agent_id.is_some(),
            "outcome":timing.outcome, "prompt_bytes":timing.prompt_bytes,
            "output_bytes":timing.output_bytes, "soft_budget_secs":timing.soft_budget_secs,
            "hard_timeout_secs":timing.hard_timeout_secs,
            "dispatch_to_registration_ms":interval(Some(timing.offered_at_ms), timing.registered_at_ms),
            "registration_to_finish_ms":interval(timing.registered_at_ms, timing.finished_at_ms),
            "dispatch_to_finish_ms":interval(Some(timing.offered_at_ms), timing.finished_at_ms),
        }));
    }
    Ok(serde_json::json!({
        "evidence":"host-observed wall-clock intervals include scheduling, spawn/follow-up and result relay; not pure model execution time",
        "limits":"null is unknown or clock reversal; a deadline records a stop request, not confirmed termination; missing usage is not zero cost",
        "requests":records,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_and_missing_observations_are_not_invented() {
        assert_eq!(interval(Some(10), Some(35)), Some(25));
        assert_eq!(interval(Some(35), Some(10)), None);
        assert_eq!(interval(None, Some(35)), None);
        assert_eq!(interval(Some(i64::MIN), Some(i64::MAX)), None);
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        super::super::tests::request(&store, "a", "luna");
        let report = summary(&store).unwrap();
        assert_eq!(report["requests"][0]["observation"], "unknown");
        let value = serde_json::json!({"dispatch_id":"a","role":"recommender",
            "prompt_bytes":12,"soft_budget_secs":60,"hard_timeout_secs":180,
            "offered_at_ms":100,"registered_at_ms":120,"finished_at_ms":170,
            "outcome":"completed","output_bytes":24});
        store
            .write_artifact("native-timing-a.json", &value.to_string())
            .unwrap();
        let report = summary(&store).unwrap();
        assert_eq!(report["requests"][0]["dispatch_to_registration_ms"], 20);
        assert_eq!(report["requests"][0]["registration_to_finish_ms"], 50);
        assert_eq!(report["requests"][0]["dispatch_to_finish_ms"], 70);
        let mut wrong = value;
        wrong["dispatch_id"] = serde_json::json!("different");
        store
            .write_artifact("native-timing-a.json", &wrong.to_string())
            .unwrap();
        assert!(summary(&store).is_err());
    }
}
