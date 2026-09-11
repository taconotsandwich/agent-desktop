use super::*;

#[tokio::test]
#[ignore = "requires an isolated Linux desktop with Blender"]
async fn qa_blender_workflow() -> Result<()> {
    let seat = Seat::boot("blender-workflow").await?;
    eprintln!("QA artifacts: {}", seat.artifacts.display());
    let mut client = Joiner::spawn(
        &server_bin(),
        &seat.env_file,
        &seat.artifacts.join("server.log"),
    )
    .await?;
    crate::support::blender::workflow(
        &mut client,
        &seat.artifacts,
        &seat.environment,
        "Desktop QA",
        2,
    )
    .await?;
    client.close().await?;
    Ok(())
}
