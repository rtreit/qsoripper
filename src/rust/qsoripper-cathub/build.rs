//! Generate the shared protobuf contract used by the `CatHub` `WinKeyer` broker.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = PathBuf::from("../../../proto");
    println!("cargo::rerun-if-changed={}", proto_root.display());
    let service_root = proto_root.join("services");
    let protos: Vec<_> = [
        "abort_winkeyer_client_request.proto",
        "abort_winkeyer_client_response.proto",
        "cancel_winkeyer_job_request.proto",
        "cancel_winkeyer_job_response.proto",
        "get_winkeyer_broker_status_request.proto",
        "get_winkeyer_broker_status_response.proto",
        "send_winkeyer_text_request.proto",
        "send_winkeyer_text_response.proto",
        "set_winkeyer_broker_speed_request.proto",
        "set_winkeyer_broker_speed_response.proto",
        "stream_winkeyer_events_request.proto",
        "stream_winkeyer_events_response.proto",
        "winkeyer_broker_event_kind.proto",
        "winkeyer_broker_service.proto",
        "winkeyer_broker_status.proto",
        "winkeyer_speed_mode.proto",
    ]
    .into_iter()
    .map(|file| service_root.join(file))
    .collect();
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&protos, &[proto_root])?;
    Ok(())
}
