use super::*;
use ditto_protocol::NewEvent;

fn requested() -> NewEvent {
    let command = RepeatScheduleCommand {
        request_id: ulid::Ulid::new().to_string(),
        session_id: "personal".into(),
        text: "read scoped evidence".into(),
        due_at: "2026-09-09T00:00:00Z".parse().unwrap(),
        expires_at: "2026-09-09T00:00:30Z".parse().unwrap(),
        every_seconds: 60,
        occurrences: 1000,
    };
    NewEvent {
        session_id: Some(command.session_id.clone()),
        task_id: Some(format!("repeat_{}", command.request_id)),
        actor: EventActor::User,
        kind: REPEAT_REQUESTED.into(),
        payload: json!({"version":1,"command":command}),
        causation_id: None,
        correlation_id: None,
        span_id: None,
    }
}
fn entry(store: &EventStore, source: &EventRecord) -> RepeatEntry {
    store
        .repeat_entry(
            "personal",
            source.payload["command"]["request_id"].as_str().unwrap(),
        )
        .unwrap()
        .unwrap()
}
fn claim(parent: &RepeatEntry, child: &str, run: &str) -> NewEvent {
    NewEvent {
        session_id: Some(parent.session_id.clone()),
        task_id: Some(format!("schedule_{child}")),
        actor: EventActor::Scheduler,
        kind: OCCURRENCE_CLAIMED.into(),
        payload: json!({"version":1,"parent_request_id":parent.request_id,
            "source_event_id":parent.source_event_id,"occurrence":parent.next_occurrence,
            "run_request_id":run,"observed_at_ms":parent.timing.window(parent.next_occurrence).unwrap().0,
            "progress":parent.claim_progress(child)}),
        causation_id: Some(parent.last_event_id.clone()),
        correlation_id: None,
        span_id: None,
    }
}
fn skip(parent: &RepeatEntry, through: u32) -> NewEvent {
    NewEvent {
        session_id: Some(parent.session_id.clone()),
        task_id: Some(format!("repeat_{}", parent.request_id)),
        actor: EventActor::Scheduler,
        kind: REPEAT_SKIPPED.into(),
        payload: json!({"version":1,"source_event_id":parent.source_event_id,
            "from_occurrence":parent.next_occurrence,"through_occurrence":through,
            "observed_at_ms":parent.timing.window(through).unwrap().1,
            "progress":parent.skip_progress(through).unwrap()}),
        causation_id: Some(parent.last_event_id.clone()),
        correlation_id: None,
        span_id: None,
    }
}

#[test]
fn fixed_anchor_expiry_is_exclusive_and_extreme_downtime_is_bounded() {
    let source = requested();
    let command = serde_json::from_value(source.payload["command"].clone()).unwrap();
    let timing = RepeatTiming::new(&command).unwrap();
    assert_eq!(timing.expired_through(i64::MIN).unwrap(), 0);
    assert_eq!(
        timing.expired_through(timing.first_expiry_ms - 1).unwrap(),
        0
    );
    assert_eq!(timing.expired_through(timing.first_expiry_ms).unwrap(), 1);
    let (due, expiry) = timing.window(999).unwrap();
    assert_eq!(due, timing.first_due_ms + 998 * 60_000);
    assert_eq!(timing.expired_through(due).unwrap(), 998);
    assert_eq!(timing.expired_through(expiry).unwrap(), 999);
    assert_eq!(timing.expired_through(i64::MAX).unwrap(), 1000);
    assert!(timing.window(0).is_err());
    assert!(timing.window(1001).is_err());
    let corrupt = RepeatTiming {
        interval_ms: 0,
        ..timing
    };
    assert!(corrupt.expired_through(i64::MAX).is_err());
    assert!(corrupt.window(1).is_err());
}

#[test]
fn claim_journal_child_and_parent_progress_commit_or_rollback_together() {
    let root = tempfile::tempdir().unwrap();
    let store = EventStore::open(root.path().join("events.db")).unwrap();
    let source = store.append(requested()).unwrap();
    let parent = entry(&store, &source);
    let child = ulid::Ulid::new().to_string();
    let run = ulid::Ulid::new().to_string();
    let valid = claim(&parent, &child, &run);
    for field in [
        "progress",
        "occurrence",
        "observed_at_ms",
        "parent_request_id",
        "source_event_id",
    ] {
        let mut invalid = valid.clone();
        invalid.payload[field] = json!(0);
        assert!(store.append(invalid).is_err(), "{field}");
        assert_eq!(store.count().unwrap(), 1);
        assert!(store.schedule_entry("personal", &child).unwrap().is_none());
        assert!(!store.is_scheduled_run(&run).unwrap());
        assert_eq!(entry(&store, &source).progress(), parent.progress());
    }
    for variation in 0..4 {
        let mut invalid = valid.clone();
        match variation {
            0 => invalid.actor = EventActor::User,
            1 => invalid.session_id = Some("other".into()),
            2 => invalid.causation_id = None,
            _ => invalid.correlation_id = Some("turn_forged".into()),
        }
        assert!(store.append(invalid).is_err());
        assert_eq!(store.count().unwrap(), 1);
    }
    let claimed = store.append(valid.clone()).unwrap();
    assert!(store.append(valid).is_err());
    assert_eq!(store.count().unwrap(), 2);
    assert_eq!(entry(&store, &source).claimed, 1);
    let indexed = store.schedule_entry("personal", &child).unwrap().unwrap();
    assert_eq!(indexed.state, crate::ScheduleState::Claimed);
    assert_eq!(indexed.source_event_id, claimed.event_id);
    let derived = store.occurrence_source(&indexed, &claimed).unwrap();
    assert_eq!(derived.text, "read scoped evidence");
    let parent = entry(&store, &source);
    // A unique run reservation failure must not consume the next ordinal.
    assert!(
        store
            .append(claim(&parent, &ulid::Ulid::new().to_string(), &run))
            .is_err()
    );
    assert_eq!(store.count().unwrap(), 2);
    assert_eq!(entry(&store, &source).progress(), parent.progress());
}

#[test]
fn deleted_indexes_rebuild_mixed_claims_missed_ranges_and_cancellation_from_journal() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("events.db");
    let store = EventStore::open(&path).unwrap();
    let source = store.append(requested()).unwrap();
    store.append(skip(&entry(&store, &source), 998)).unwrap();
    let child = ulid::Ulid::new().to_string();
    let run = ulid::Ulid::new().to_string();
    let claimed = store
        .append(claim(&entry(&store, &source), &child, &run))
        .unwrap();
    let parent = entry(&store, &source);
    store.append(NewEvent {
        session_id: source.session_id.clone(), task_id: source.task_id.clone(),
        actor: EventActor::User, kind: REPEAT_CANCELLED.into(),
        payload: json!({"version":1,"source_event_id":source.event_id,"progress":parent.progress()}),
        causation_id: Some(claimed.event_id.clone()), correlation_id: None, span_id: None,
    }).unwrap();
    let before = entry(&store, &source);
    store
        .connection()
        .unwrap()
        .execute_batch("DROP TABLE repeat_index; DROP TABLE schedule_index;")
        .unwrap();
    drop(store);
    let store = EventStore::open(path).unwrap();
    let restored = entry(&store, &source);
    assert_eq!(restored.progress(), before.progress());
    assert_eq!(restored.state, RepeatState::Cancelled);
    assert_eq!(restored.missed, 998);
    assert_eq!(restored.claimed, 1);
    assert_eq!(restored.next_occurrence, 1000);
    assert_eq!(store.count().unwrap(), 4);
    assert_eq!(store.future_schedule_count().unwrap(), 0);
    assert!(store.active_repeats().unwrap().is_empty());
    store.verified_repeat(&restored).unwrap();
    let indexed = store.schedule_entry("personal", &child).unwrap().unwrap();
    store.occurrence_source(&indexed, &claimed).unwrap();
    assert!(store.is_scheduled_run(&run).unwrap());
    assert!(
        store
            .append(claim(
                &restored,
                &ulid::Ulid::new().to_string(),
                &ulid::Ulid::new().to_string()
            ))
            .is_err()
    );
}

#[test]
fn cached_progress_and_child_scope_are_bound_to_immutable_records() {
    let root = tempfile::tempdir().unwrap();
    let store = EventStore::open(root.path().join("events.db")).unwrap();
    let source = store.append(requested()).unwrap();
    let child = ulid::Ulid::new().to_string();
    let claimed = store
        .append(claim(
            &entry(&store, &source),
            &child,
            &ulid::Ulid::new().to_string(),
        ))
        .unwrap();
    let parent = entry(&store, &source);
    store.verified_repeat(&parent).unwrap();
    for change in 0..5 {
        let mut drifted = parent.clone();
        match change {
            0 => {
                drifted.claimed = 0;
                drifted.missed = 1;
                drifted.last_child_id = None;
            }
            1 => drifted.state = RepeatState::Cancelled,
            2 => drifted.last_child_id = Some(ulid::Ulid::new().to_string()),
            3 => drifted.timing.first_due_ms += 1,
            _ => drifted.session_id = "other".into(),
        }
        assert!(store.verified_repeat(&drifted).is_err());
    }
    let child = store.schedule_entry("personal", &child).unwrap().unwrap();
    for change in 0..4 {
        let mut drifted = child.clone();
        match change {
            0 => drifted.session_id = "other".into(),
            1 => drifted.request_id = ulid::Ulid::new().to_string(),
            2 => drifted.run_request_id = ulid::Ulid::new().to_string(),
            _ => drifted.due_at_ms += 1,
        }
        assert!(store.occurrence_source(&drifted, &claimed).is_err());
    }
}

#[test]
fn old_schema_five_one_shots_migrate_without_rewriting_source_or_reservations() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("events.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(crate::MIGRATION_V1).unwrap();
    db.execute_batch(crate::MIGRATION_V2).unwrap();
    db.execute_batch(crate::MIGRATION_V3).unwrap();
    db.execute_batch(crate::MIGRATION_V4).unwrap();
    db.execute_batch(crate::schedule::TABLE_SCHEMA).unwrap();
    db.execute_batch(crate::schedule::MIGRATION_V5).unwrap();
    db.pragma_update(None, "user_version", 5).unwrap();
    let run = ulid::Ulid::new().to_string();
    let repeat = requested();
    let mut command = repeat.payload["command"].clone();
    command.as_object_mut().unwrap().remove("every_seconds");
    command.as_object_mut().unwrap().remove("occurrences");
    let request = command["request_id"].as_str().unwrap().to_owned();
    let source = crate::insert_event(
        &db,
        NewEvent {
            task_id: Some(format!("schedule_{request}")),
            kind: "schedule.requested".into(),
            payload: json!({"version":1,"command":command,"run_request_id":run}),
            ..repeat
        },
    )
    .unwrap();
    // The old pending cache is dispensable even during migration.
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        5
    );
    drop(db);
    let store = EventStore::open(path).unwrap();
    assert_eq!(
        serde_json::to_value(store.get_by_event_id(&source.event_id).unwrap().unwrap()).unwrap(),
        serde_json::to_value(source).unwrap()
    );
    assert_eq!(store.count().unwrap(), 1);
    assert_eq!(store.future_schedule_count().unwrap(), 1);
    assert_eq!(store.pending_schedules().unwrap()[0].request_id, request);
    assert!(store.is_scheduled_run(&run).unwrap());
    assert!(store.active_repeats().unwrap().is_empty());
    assert_eq!(
        store
            .connection()
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        6
    );
}

#[test]
fn active_headers_capacity_and_streamed_rebuild_use_indexes_without_temporary_sort() {
    let root = tempfile::tempdir().unwrap();
    let store = EventStore::open(root.path().join("events.db")).unwrap();
    let db = store.connection().unwrap();
    for (query, indexes) in [
        (
            "SELECT * FROM repeat_index INDEXED BY repeat_active_due WHERE state = 'active' ORDER BY first_due_ms + (next_occurrence - 1) * interval_ms,session_id,request_id LIMIT 101",
            vec!["repeat_active_due"],
        ),
        (
            "SELECT * FROM repeat_index WHERE session_id = 'personal' AND request_id = 'id'",
            vec!["SEARCH repeat_index"],
        ),
        (
            "SELECT COUNT(*) FROM (SELECT 1 FROM schedule_index WHERE state = 'pending' UNION ALL SELECT 1 FROM repeat_index WHERE state = 'active' LIMIT 101)",
            vec!["schedule_pending", "repeat_active_due"],
        ),
        (
            "SELECT seq FROM events INDEXED BY events_schedules WHERE kind GLOB 'schedule.*' ORDER BY seq",
            vec!["events_schedules"],
        ),
    ] {
        let plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join(" ");
        for index in indexes {
            assert!(plan.contains(index), "{plan}");
        }
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    }
}
