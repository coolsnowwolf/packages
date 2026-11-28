use super::{Queue, MAX_PENDING_BYTES};

#[test]
fn mqtt_telemetry_slow_consumer_pending_budget_and_eventual_drain() {
    let mut queue = Queue::default();
    let mut peak = 0;
    // Hold the parser back while many maximum reports and invalid sources arrive.
    for n in 0..200u64 {
        assert!(queue.put(
            &format!("02000000{n:04X}"),
            vec![7; super::super::archive::MAX_COMPRESSED],
            n,
            &[]
        ));
        assert!(!queue.put("../../invalid", vec![0; 32], n, &[]));
        let bytes = queue
            .pending
            .values()
            .filter_map(|p| p.payload.as_ref())
            .map(|p| p.len())
            .sum::<usize>();
        peak = peak.max(bytes);
        assert!(bytes <= MAX_PENDING_BYTES);
        assert!(queue.seen.len() <= 64);
    }
    assert_eq!(peak, MAX_PENDING_BYTES);
    let mut count = 0;
    while queue.take().is_some() {
        count += 1;
    }
    assert_eq!(count, 64);
    assert!(queue.pending.is_empty());
    assert!(queue.put("020000000001", vec![1], 201, &[]));
    assert_eq!(queue.take().unwrap().1.payload.as_deref(), Some(&[1][..]));
    println!(
        "pending_peak_bytes={peak}, after_drain_bytes=0, retained_sources={}",
        queue.seen.len()
    );
}

#[test]
fn evicted_receipt_completes_without_losing_last_good_report() {
    let mut store = super::Store::default();
    let mac = "020000000001";
    let mut state = super::State::default();
    state.apply(
        0,
        Ok(super::super::parse(&super::super::tests::fixture(&[("IP", b"192.0.2.10")])).unwrap()),
    );
    store.states.insert(mac.into(), state);
    let size = super::super::archive::MAX_COMPRESSED;
    for received in 1..=5 {
        assert!(store.queue.put(
            &format!("02000000{received:04X}"),
            vec![1; size],
            received,
            &[]
        ));
    }

    let next = store.take().unwrap();

    assert_eq!(next.0, "020000000002");
    let state = &store.states[mac];
    assert_eq!(state.last_processed_receipt, Some(1));
    assert_eq!(state.last_good, Some(0));
    assert_eq!(
        state.report.as_ref().unwrap().fields["ip"].value,
        "192.0.2.10"
    );
    assert_eq!(
        state.error.as_deref(),
        Some("pending telemetry byte budget exceeded")
    );
}

#[test]
fn oldest_payload_becomes_error_receipt_when_pending_budget_is_exceeded() {
    let mut queue = Queue::default();
    let size = super::super::archive::MAX_COMPRESSED;
    for received in 1..=4 {
        assert!(queue.put(
            &format!("02000000{received:04X}"),
            vec![1; size],
            received,
            &[]
        ));
    }

    assert!(queue.put("020000000005", vec![2; size], 5, &[]));

    assert_eq!(
        queue
            .pending
            .values()
            .filter_map(|item| item.payload.as_ref())
            .map(|p| p.len())
            .sum::<usize>(),
        MAX_PENDING_BYTES
    );
    let (mac, oldest) = queue.take().unwrap();
    assert_eq!(mac, "020000000001");
    assert_eq!(oldest.received, 1);
    assert!(oldest.budget_evicted);
    assert!(oldest.payload.is_none());
    assert_eq!(
        queue.pending["020000000005"].payload.as_deref(),
        Some(vec![2; size].as_slice())
    );
}

#[test]
fn replacement_reuses_budget_when_source_already_has_payload() {
    let mut queue = Queue::default();
    let size = super::super::archive::MAX_COMPRESSED;
    for received in 1..=4 {
        assert!(queue.put(
            &format!("02000000{received:04X}"),
            vec![1; size],
            received,
            &[]
        ));
    }

    assert!(queue.put("020000000001", vec![2; size], 5, &[]));

    assert_eq!(queue.pending.len(), 4);
    assert!(queue.pending.values().all(|item| !item.budget_evicted));
    assert_eq!(queue.pending["020000000001"].received, 5);
}

#[test]
fn processing_payload_is_not_evicted_when_more_pending_payloads_arrive() {
    let mut queue = Queue::default();
    let size = super::super::archive::MAX_COMPRESSED;
    assert!(queue.put("020000000001", vec![7; size], 1, &[]));
    let processing = queue.take().unwrap().1;

    for received in 2..=6 {
        assert!(queue.put(
            &format!("02000000{received:04X}"),
            vec![1; size],
            received,
            &[]
        ));
    }

    assert_eq!(
        processing.payload.as_deref(),
        Some(vec![7; size].as_slice())
    );
    assert_eq!(
        queue.used.load(std::sync::atomic::Ordering::Relaxed),
        MAX_PENDING_BYTES
    );
    assert!(queue.pending["020000000003"].budget_evicted);
    drop(processing);
    assert_eq!(
        queue.used.load(std::sync::atomic::Ordering::Relaxed),
        MAX_PENDING_BYTES - size
    );
    assert!(queue.pending["020000000002"].budget_evicted);
    assert!(!queue.pending["020000000006"].budget_evicted);
}

#[test]
fn retained_budget_rejects_growth_without_discarding_last_good_report() {
    let mut store = super::Store::default();
    let parse =
        || super::super::parse(&super::super::tests::fixture(&[("IP", b"192.0.2.10")])).unwrap();
    store.complete(
        "020000000001".into(),
        1,
        Ok((parse(), super::MAX_REPORT_BYTES - 1)),
    );
    store.complete("020000000002".into(), 1, Ok((parse(), 1)));
    store.complete("020000000002".into(), 2, Ok((parse(), 2)));
    let old = &store.states["020000000002"];
    assert_eq!(old.last_good, Some(1));
    assert_eq!(old.last_processed_receipt, Some(2));
    assert_eq!(
        old.error.as_deref(),
        Some("retained telemetry report budget exceeded")
    );
    assert_eq!(
        store.report_bytes.values().sum::<usize>(),
        super::MAX_REPORT_BYTES
    );
}
