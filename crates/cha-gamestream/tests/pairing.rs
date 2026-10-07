//! Pairing, driven by moonlight-common-rust as a real client.

mod common;

use std::time::Duration;

use cha_gamestream::PairingStore;
use common::{Rig, TestClient};

#[tokio::test(flavor = "multi_thread")]
async fn a_client_pairs_with_the_pin_the_directory_supplies_and_lists_apps() {
    let rig = Rig::start().await;
    let client = TestClient::new("client-one");
    let host = client.host(&rig);

    client.pair(&rig, &host, "4321", "4321").await.unwrap();

    // The store has the client, by its certificate, with what it said of itself.
    let paired = rig.store.list().await.unwrap();
    assert_eq!(paired.len(), 1);
    assert_eq!(paired[0].client, client.id());
    assert_eq!(paired[0].unique_id, "client-one");
    assert_eq!(paired[0].name, "TestDevice");
    // The directory was asked, once, who wanted a PIN.
    let attempts = rig.directory.attempts();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].client_unique_id, "client-one");
    assert_eq!(attempts[0].client, client.id());

    // With its certificate, the client sees the host's apps (over HTTPS, paired).
    host.update().await.unwrap();
    let apps = host.app_list().await.unwrap();
    assert_eq!(
        apps.iter().map(|a| a.title.as_str()).collect::<Vec<_>>(),
        ["Desktop", "Steam & Co"]
    );
    assert_eq!(apps.iter().map(|a| a.id.0).collect::<Vec<_>>(), [1, 2]);
    assert!(apps[1].is_hdr_supported && !apps[0].is_hdr_supported);
    assert_eq!(host.host_name().await.unwrap(), "Test Host");

    // /unpair removes the client through the store, and its certificate stops working.
    host.unpair().await.unwrap();
    assert!(rig.store.list().await.unwrap().is_empty());
    assert!(!rig.store.is_paired(&client.id()).await.unwrap());
    // (`update` asks the host again; the client's cache would still show the list.)
    let after = host.update().await;
    assert!(
        after.is_err(),
        "an unpaired certificate must not list apps: {after:?}"
    );
    rig.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_pin_pairs_nobody() {
    let rig = Rig::start().await;
    let client = TestClient::new("client-wrong");
    let host = client.host(&rig);

    let outcome = client.pair(&rig, &host, "1111", "9999").await;
    assert!(
        outcome.is_err(),
        "the client must refuse a host that had another PIN"
    );
    assert!(rig.store.list().await.unwrap().is_empty());
    // And it has no access.
    assert!(!host.is_paired().await.unwrap());
    rig.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pairing_waits_for_the_directory_and_gives_up_without_a_pin() {
    let rig = Rig::with(|c| c.pin_timeout = Duration::from_millis(300)).await;
    let client = TestClient::new("client-slow");
    let host = client.host(&rig);
    let (cert, key) = client.ids();
    let pin = moonlight_common::http::pair::PairPin::new(1, 2, 3, 4).unwrap();
    // Nobody types the PIN anywhere.
    let outcome = host
        .pair(
            &cert,
            &key,
            "Slow".into(),
            pin,
            moonlight_common::crypto::rustcrypto::RustCryptoBackend,
        )
        .await;
    assert!(outcome.is_err());
    assert!(rig.store.list().await.unwrap().is_empty());
    assert_eq!(rig.directory.attempts().len(), 1);
    rig.host.shutdown().await;
}

/// There is no way to give a PIN over HTTP: the old Moonshine pages are gone.
#[tokio::test(flavor = "multi_thread")]
async fn the_pin_cannot_be_submitted_over_http() {
    let rig = Rig::start().await;
    for (method, path) in [
        ("GET", "/pin?uniqueid=0123456789ABCDEF"),
        ("POST", "/submit-pin"),
        ("GET", "/submit-pin?uniqueid=a&pin=1234"),
    ] {
        let status = common::raw_http(rig.http_port(), &format!("{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")).await;
        assert!(
            status.starts_with("HTTP/1.1 404"),
            "{method} {path}: {status}"
        );
    }
    rig.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn protected_endpoints_refuse_unpaired_and_certificate_less_clients() {
    let rig = Rig::start().await;
    // Plain HTTP has no such endpoints at all.
    for path in ["/applist", "/launch?appid=1", "/cancel", "/resume"] {
        let status = common::raw_http(
            rig.http_port(),
            &format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"),
        )
        .await;
        assert!(status.starts_with("HTTP/1.1 404"), "{path}: {status}");
    }
    // Over HTTPS, a client that was never paired is refused.
    let stranger = TestClient::new("stranger");
    let host = stranger.host(&rig);
    let (cert, key) = stranger.ids();
    let server = common::server_identifier(&rig);
    // Loading the identity reads the app list too, which this certificate may not.
    let refused = host.set_identity(cert, key, server).await;
    assert!(refused.is_err(), "an unpaired certificate got the app list");
    assert!(rig.directory.launches().is_empty());
    rig.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_unpair_can_be_switched_off() {
    let rig = Rig::with(|c| c.unauthenticated_unpair = false).await;
    let client = TestClient::new("client-keep");
    client.paired_host(&rig).await;
    // An unauthenticated HTTP /unpair names the client by its unique id and does nothing now.
    common::raw_http(
        rig.http_port(),
        "GET /unpair?uniqueid=client-keep HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(rig.store.list().await.unwrap().len(), 1);
    rig.host.shutdown().await;
}
