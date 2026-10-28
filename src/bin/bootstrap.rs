use lambda_runtime::{LambdaEvent, service_fn};
use serde_json::{Value, json};

#[tokio::main]
async fn main() -> Result<(), lambda_runtime::Error> {
    lambda_runtime::run(service_fn(|event:LambdaEvent<Value>|async move{
        let worker=event.payload.get("Records").is_some();
        match promptlist::web::serverless::handle(event.payload).await {
            Ok(value)=>Ok::<_,lambda_runtime::Error>(value),
            Err(error)=>{
                eprintln!("Promptlist invocation failed: {error}");
                // An errored invocation must not freeze and later resume a
                // spawned Spotify writer under an expired account lease.
                if worker{std::process::exit(1);}
                Ok(json!({"statusCode":503,"headers":{"content-type":"text/plain","cache-control":"no-store"},"body":"The request could not be confirmed. Check saved state before retrying; no automatic resubmission."}))
            }
        }
    })).await
}
