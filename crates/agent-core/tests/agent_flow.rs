//! End-to-end orchestrator flow without a bus: ask() -> mock provider ->
//! persisted turns, via a signal recorder installed on the store side.

use std::sync::{Arc, Mutex};

use agent_core::agent::Agent;
use agent_core::config::Config;
use agent_core::provider::MockProvider;
use agent_core::store::Store;

fn cfg() -> Config {
    toml::from_str(
        r#"
[provider]
kind = "openai"
model = "m"
api_key = "k"
"#,
    )
    .unwrap()
}

#[tokio::test]
async fn ask_persists_both_turns() {
    let dir = std::env::temp_dir().join(format!("aisle-flow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("s.db")).unwrap();
    let agent = Agent::for_test(cfg(), store, Arc::new(MockProvider { reply: "the answer".into(), chunk_every: 3 }));

    let s = agent.create_session("", None).unwrap();
    let rid = agent.ask(&s.id, "the question", &Default::default()).await.unwrap();
    assert!(!rid.is_empty());

    // wait for the background task to settle (poll the store)
    for _ in 0..100 {
        let msgs = agent.get_messages(&s.id, 10).unwrap();
        if msgs.len() >= 2 {
            assert_eq!(msgs[0].role, "user");
            assert_eq!(msgs[0].text, "the question");
            assert_eq!(msgs[1].role, "assistant");
            assert_eq!(msgs[1].text, "the answer");
            std::fs::remove_dir_all(dir).ok();
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("assistant turn never persisted");
}

#[tokio::test]
async fn attach_screen_fails_cleanly_without_backends() {
    let dir = std::env::temp_dir().join(format!("aisle-nocap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("s.db")).unwrap();
    let agent = Agent::for_test(cfg(), store, Arc::new(MockProvider { reply: "x".into(), chunk_every: 10 }));
    let s = agent.create_session("", None).unwrap();

    let mut opts = std::collections::HashMap::new();
    opts.insert("attach_screen".into(), "true".into());
    let err = agent.ask(&s.id, "with screen", &opts).await.unwrap_err();
    assert_eq!(err.kind(), "capture");
    // nothing was persisted (half-truth rule)
    assert!(agent.get_messages(&s.id, 10).unwrap().is_empty());
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn frontend_registration_first_wins() {
    let dir = std::env::temp_dir().join(format!("aisle-fe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let agent = Agent::for_test(cfg(), Store::open(&dir.join("s.db")).unwrap(), Arc::new(MockProvider { reply: String::new(), chunk_every: 1 }));
    assert!(agent.register_frontend("gtk"));
    assert!(!agent.register_frontend("extension")); // first registration wins
    std::fs::remove_dir_all(dir).ok();
}
