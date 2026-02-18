use sayiir_runtime::prelude::*;
use sayiir_core::error::BoxError;

#[task]
async fn greet(name: String) -> Result<String, BoxError> {
    Ok(format!("Hello, {}!", name))
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let workflow = workflow! {
        name: "hello",
        steps: [greet]
    }
    .unwrap();

    let status = workflow.run_once("World".to_string()).await?;

    println!("{:?}", status);
    Ok(())
}
