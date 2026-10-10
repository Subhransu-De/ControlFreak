use super::*;
use super::{outcome_tests::exchange, stop_tests::WaitingBackend};
use tokio::io::{AsyncWriteExt, BufReader};

fn metadata() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "protocol-fixture", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

fn start_server() -> (
    BufReader<tokio::io::DuplexStream>,
    tokio::task::JoinHandle<()>,
    SafetyIndicator,
) {
    let indicator = SafetyIndicator::dormant();
    let server = ControlFreakServer::with_indicator(
        Arc::new(WaitingBackend::idle()),
        indicator.clone(),
        None,
    );
    let (client, server_io) = tokio::io::duplex(256 * 1024);
    let serving = tokio::spawn(async move {
        server
            .serve(server_io)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    });
    (BufReader::new(client), serving, indicator)
}

async fn finish(client: BufReader<tokio::io::DuplexStream>, serving: tokio::task::JoinHandle<()>) {
    drop(client);
    tokio::time::timeout(Duration::from_secs(5), serving)
        .await
        .expect("server shuts down after transport closes")
        .unwrap();
}

#[tokio::test]
async fn discovery_and_rejected_bootstrap_requests_allow_classic_initialization() {
    let mut malformed = metadata();
    malformed
        .as_object_mut()
        .unwrap()
        .remove("io.modelcontextprotocol/clientCapabilities");
    let mut unsupported = metadata();
    unsupported["io.modelcontextprotocol/protocolVersion"] = json!("2099-99-99");
    for (probe, succeeds) in [
        (
            json!({"jsonrpc":"2.0", "id":1, "method":"server/discover", "params":{"_meta":metadata()}}),
            true,
        ),
        (
            json!({"jsonrpc":"2.0", "id":1, "method":"server/discover", "params":{"_meta":malformed}}),
            false,
        ),
        (
            json!({"jsonrpc":"2.0", "id":1, "method":"tools/list", "params":{"_meta":unsupported}}),
            false,
        ),
        (
            json!({"jsonrpc":"2.0", "id":1, "method":"tools/list", "params":{}}),
            false,
        ),
    ] {
        let (mut client, serving, indicator) = start_server();
        let response = exchange(&mut client, probe).await;
        assert_eq!(response.get("error").is_none(), succeeds, "{response}");
        assert_eq!(indicator.status(), "dormant");

        let initialized = exchange(
            &mut client,
            json!({
                "jsonrpc":"2.0", "id":2, "method":"initialize", "params":{
                    "protocolVersion":"2025-11-25", "capabilities":{},
                    "clientInfo":{"name":"protocol-fixture", "version":"1"}
                }
            }),
        )
        .await;
        assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
        client
            .get_mut()
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();
        let listed = exchange(
            &mut client,
            json!({
                "jsonrpc":"2.0", "id":3, "method":"tools/list", "params":{}
            }),
        )
        .await;
        assert_eq!(
            listed["result"]["tools"],
            serde_json::to_value(tools()).unwrap()
        );
        for modern_field in ["ttlMs", "cacheScope", "resultType"] {
            assert!(listed["result"].get(modern_field).is_none(), "{listed}");
        }
        assert_eq!(indicator.status(), "dormant");
        finish(client, serving).await;
    }
}

#[tokio::test]
async fn inline_tool_list_after_discovery_has_conservative_cache_hints() {
    let (mut client, serving, indicator) = start_server();
    for id in 1..=2 {
        let discovered = exchange(
            &mut client,
            json!({
                "jsonrpc":"2.0", "id":id, "method":"server/discover",
                "params":{"_meta":metadata()}
            }),
        )
        .await;
        assert!(discovered.get("error").is_none(), "{discovered}");
        assert_eq!(
            discovered["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "controlfreak"
        );
    }
    let listed = exchange(
        &mut client,
        json!({
            "jsonrpc":"2.0", "id":3, "method":"tools/list", "params":{"_meta":metadata()}
        }),
    )
    .await;
    assert_eq!(
        listed["result"]["tools"],
        serde_json::to_value(tools()).unwrap()
    );
    assert_eq!(listed["result"]["ttlMs"], 0);
    assert_eq!(listed["result"]["cacheScope"], "private");
    assert_eq!(indicator.status(), "dormant");
    finish(client, serving).await;
}
