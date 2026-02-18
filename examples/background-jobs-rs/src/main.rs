use sayiir_runtime::prelude::*;
use sayiir_core::error::BoxError;

#[task(timeout = "10s")]
async fn fetch_recipients(campaign_id: String) -> Result<Vec<String>, BoxError> {
    // In production: query database
    Ok(vec![
        "alice@example.com".into(),
        "bob@example.com".into(),
    ])
}

#[task(timeout = "30s", retries = 3, backoff = "1s")]
async fn send_emails(recipients: Vec<String>) -> Result<String, BoxError> {
    // In production: call email service
    Ok(format!("Sent to {} recipients", recipients.len()))
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let workflow = workflow! {
        name: "email-pipeline",
        steps: [fetch_recipients, send_emails]
    }
    .unwrap();

    let status = workflow.run_once("summer-sale".to_string()).await?;

    println!("{:?}", status);
    Ok(())
}
