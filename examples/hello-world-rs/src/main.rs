use sayiir_runtime::prelude::*;
use sayiir_core::error::BoxError;

#[task]
async fn greet(name: String) -> Result<String, BoxError> {
    Ok(format!("Hello, {}!", name))
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let workflow = workflow!("hello", JsonCodec, TaskRegistry::new(),
        greet
    )
    .unwrap();

    let backend = InMemoryBackend::new();
    let runner = CheckpointingRunner::new(backend);
    let status = runner
        .run(workflow.workflow(), "hello-001", "World".to_string())
        .await?;

    println!("{:?}", status);
    Ok(())
}
