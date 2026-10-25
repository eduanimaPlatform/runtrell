use runtrell::{memory::MemoryStore, *};
#[test]
fn reservation_is_idempotent_and_unknown_is_not_zero() {
    runtrell_test_run(async {
        let s = MemoryStore::default();
        let account = s
            .create_account("t", Amount::new(100, 100), None)
            .await
            .unwrap();
        let spec = RunSpec::new("t", "request", account.clone(), "judge");
        let h = s.start(spec.clone()).await.unwrap();
        assert_eq!(s.start(spec).await.unwrap(), h);
        let claim = s.claim(&h, "worker", 0, 10).await.unwrap();
        let request = DispatchRequest::new(
            "op",
            OperationKind::Model,
            "fake",
            serde_json::json!({}),
            Amount::new(60, 20),
        );
        let op = s.begin(&claim, request.clone(), 0).await.unwrap();
        assert_eq!(s.begin(&claim, request, 0).await.unwrap(), op);
        s.record(&claim, &op.id, ExternalResult::Unknown, 0)
            .await
            .unwrap();
        let balance = s.account(&account).await.unwrap();
        assert_eq!(balance.reserved, Amount::new(60, 20));
        assert_eq!(balance.used, Amount::default());
        assert_eq!(balance.unknown, 1);
        assert!(
            s.begin(
                &claim,
                DispatchRequest::new(
                    "next",
                    OperationKind::Tool,
                    "x",
                    serde_json::json!({}),
                    Amount::new(1, 1)
                ),
                0
            )
            .await
            .is_err()
        );
    });
}
fn runtrell_test_run(f: impl std::future::Future<Output = ()>) {
    let w = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(w);
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(()) = f.as_mut().poll(&mut cx) {
            break;
        }
        std::thread::yield_now();
    }
}
