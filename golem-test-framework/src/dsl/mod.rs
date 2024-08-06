// Copyright 2024 Golem Cloud
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

pub mod benchmark;

use crate::config::TestDependencies;
use anyhow::anyhow;
use async_trait::async_trait;
use golem_api_grpc::proto::golem::worker::update_record::Update;
use golem_api_grpc::proto::golem::worker::v1::worker_error::Error;
use golem_api_grpc::proto::golem::worker::v1::{
    get_worker_metadata_response, get_workers_metadata_response, interrupt_worker_response,
    invoke_and_await_response, invoke_response, launch_new_worker_response, resume_worker_response,
    update_worker_response, worker_execution_error, ConnectWorkerRequest, DeleteWorkerRequest,
    GetWorkerMetadataRequest, GetWorkersMetadataRequest, GetWorkersMetadataSuccessResponse,
    InterruptWorkerRequest, InterruptWorkerResponse, InvokeAndAwaitRequest, InvokeRequest,
    LaunchNewWorkerRequest, ResumeWorkerRequest, UpdateWorkerRequest, UpdateWorkerResponse,
    WorkerError, WorkerExecutionError,
};
use golem_api_grpc::proto::golem::worker::{
    log_event, InvokeParameters, LogEvent, StdErrLog, StdOutLog, UpdateMode,
};
use golem_common::model::oplog::{
    OplogIndex, TimestampedUpdateDescription, UpdateDescription, WorkerResourceId,
};
use golem_common::model::regions::DeletedRegions;
use golem_common::model::{
    ComponentId, ComponentVersion, FailedUpdateRecord, IdempotencyKey, ScanCursor,
    SuccessfulUpdateRecord, WorkerFilter, WorkerId, WorkerMetadata, WorkerResourceDescription,
    WorkerStatusRecord,
};
use golem_wasm_rpc::Value;
use std::collections::HashMap;
use std::path::Path;
use tokio::select;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::oneshot::Sender;
use tracing::{debug, info};
use uuid::Uuid;

#[async_trait]
pub trait TestDsl {
    async fn store_component(&self, name: &str) -> ComponentId;
    async fn store_unique_component(&self, name: &str) -> ComponentId;
    async fn store_component_unverified(&self, name: &str) -> ComponentId;
    async fn update_component(&self, component_id: &ComponentId, name: &str) -> ComponentVersion;

    async fn start_worker(&self, component_id: &ComponentId, name: &str)
        -> crate::Result<WorkerId>;
    async fn try_start_worker(
        &self,
        component_id: &ComponentId,
        name: &str,
    ) -> crate::Result<Result<WorkerId, Error>>;
    async fn start_worker_with(
        &self,
        component_id: &ComponentId,
        name: &str,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> crate::Result<WorkerId>;
    async fn try_start_worker_with(
        &self,
        component_id: &ComponentId,
        name: &str,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> crate::Result<Result<WorkerId, Error>>;
    async fn get_worker_metadata(
        &self,
        worker_id: &WorkerId,
    ) -> crate::Result<Option<WorkerMetadata>>;
    async fn get_workers_metadata(
        &self,
        component_id: &ComponentId,
        filter: Option<WorkerFilter>,
        cursor: ScanCursor,
        count: u64,
        precise: bool,
    ) -> crate::Result<(Option<ScanCursor>, Vec<WorkerMetadata>)>;
    async fn delete_worker(&self, worker_id: &WorkerId) -> crate::Result<()>;

    async fn invoke(
        &self,
        worker_id: &WorkerId,
        function_name: &str,
        params: Vec<Value>,
    ) -> crate::Result<Result<(), Error>>;
    async fn invoke_with_key(
        &self,
        worker_id: &WorkerId,
        idempotency_key: &IdempotencyKey,
        function_name: &str,
        params: Vec<Value>,
    ) -> crate::Result<Result<(), Error>>;
    async fn invoke_and_await(
        &self,
        worker_id: &WorkerId,
        function_name: &str,
        params: Vec<Value>,
    ) -> crate::Result<Result<Vec<Value>, Error>>;
    async fn invoke_and_await_with_key(
        &self,
        worker_id: &WorkerId,
        idempotency_key: &IdempotencyKey,
        function_name: &str,
        params: Vec<Value>,
    ) -> crate::Result<Result<Vec<Value>, Error>>;
    async fn invoke_and_await_custom(
        &self,
        worker_id: &WorkerId,
        function_name: &str,
        params: Vec<Value>,
    ) -> crate::Result<Result<Vec<Value>, Error>>;
    async fn invoke_and_await_custom_with_key(
        &self,
        worker_id: &WorkerId,
        idempotency_key: &IdempotencyKey,
        function_name: &str,
        params: Vec<Value>,
    ) -> crate::Result<Result<Vec<Value>, Error>>;
    async fn capture_output(&self, worker_id: &WorkerId) -> UnboundedReceiver<LogEvent>;
    async fn capture_output_forever(
        &self,
        worker_id: &WorkerId,
    ) -> (
        UnboundedReceiver<Option<LogEvent>>,
        tokio::sync::oneshot::Sender<()>,
    );
    async fn capture_output_with_termination(
        &self,
        worker_id: &WorkerId,
    ) -> UnboundedReceiver<Option<LogEvent>>;
    async fn log_output(&self, worker_id: &WorkerId);
    async fn resume(&self, worker_id: &WorkerId) -> crate::Result<()>;
    async fn interrupt(&self, worker_id: &WorkerId) -> crate::Result<()>;
    async fn simulated_crash(&self, worker_id: &WorkerId) -> crate::Result<()>;
    async fn auto_update_worker(
        &self,
        worker_id: &WorkerId,
        target_version: ComponentVersion,
    ) -> crate::Result<()>;
    async fn manual_update_worker(
        &self,
        worker_id: &WorkerId,
        target_version: ComponentVersion,
    ) -> crate::Result<()>;
}

#[async_trait]
impl<T: TestDependencies + Send + Sync> TestDsl for T {
    async fn store_component(&self, name: &str) -> ComponentId {
        let source_path = self.component_directory().join(format!("{name}.wasm"));

        let component_id = self
            .component_service()
            .get_or_add_component(&source_path)
            .await;

        let _ = log_and_save_component_metadata(&source_path).await;

        component_id
    }

    async fn store_unique_component(&self, name: &str) -> ComponentId {
        let source_path = self.component_directory().join(format!("{name}.wasm"));
        let _ = dump_component_info(&source_path);
        let uuid = Uuid::new_v4();
        let unique_name = format!("{name}-{uuid}");
        self.component_service()
            .add_component_with_name(&source_path, &unique_name)
            .await
            .expect("Failed to store unique component")
    }

    async fn store_component_unverified(&self, name: &str) -> ComponentId {
        let source_path = self.component_directory().join(format!("{name}.wasm"));
        self.component_service()
            .get_or_add_component(&source_path)
            .await
    }

    async fn update_component(&self, component_id: &ComponentId, name: &str) -> ComponentVersion {
        let source_path = self.component_directory().join(format!("{name}.wasm"));
        let _ = dump_component_info(&source_path);
        self.component_service()
            .update_component(component_id, &source_path)
            .await
    }

    async fn start_worker(
        &self,
        component_id: &ComponentId,
        name: &str,
    ) -> crate::Result<WorkerId> {
        TestDsl::start_worker_with(self, component_id, name, vec![], HashMap::new()).await
    }

    async fn try_start_worker(
        &self,
        component_id: &ComponentId,
        name: &str,
    ) -> crate::Result<Result<WorkerId, Error>> {
        TestDsl::try_start_worker_with(self, component_id, name, vec![], HashMap::new()).await
    }

    async fn start_worker_with(
        &self,
        component_id: &ComponentId,
        name: &str,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> crate::Result<WorkerId> {
        let result = TestDsl::try_start_worker_with(self, component_id, name, args, env).await?;
        Ok(result.map_err(|err| anyhow!("Failed to start worker: {err:?}"))?)
    }

    async fn try_start_worker_with(
        &self,
        component_id: &ComponentId,
        name: &str,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> crate::Result<Result<WorkerId, Error>> {
        let response = self
            .worker_service()
            .create_worker(LaunchNewWorkerRequest {
                component_id: Some(component_id.clone().into()),
                name: name.to_string(),
                args,
                env,
            })
            .await?;

        match response.result {
            None => panic!("No response from create_worker"),
            Some(launch_new_worker_response::Result::Success(response)) => Ok(Ok(response
                .worker_id
                .ok_or(anyhow!("worker_id is missing"))?
                .try_into()
                .map_err(|err: String| anyhow!(err))?)),
            Some(launch_new_worker_response::Result::Error(WorkerError { error: Some(error) })) => {
                Ok(Err(error))
            }
            Some(launch_new_worker_response::Result::Error(_)) => {
                Err(anyhow!("Error response without any details"))
            }
        }
    }

    async fn get_worker_metadata(
        &self,
        worker_id: &WorkerId,
    ) -> crate::Result<Option<WorkerMetadata>> {
        let worker_id: golem_api_grpc::proto::golem::worker::WorkerId = worker_id.clone().into();
        let response = self
            .worker_service()
            .get_worker_metadata(GetWorkerMetadataRequest {
                worker_id: Some(worker_id),
            })
            .await?;

        match response.result {
            None => Err(anyhow!("No response from connect_worker")),
            Some(get_worker_metadata_response::Result::Success(metadata)) => {
                Ok(Some(to_worker_metadata(&metadata)))
            }
            Some(get_worker_metadata_response::Result::Error(WorkerError {
                error: Some(Error::NotFound { .. }),
            })) => Ok(None),
            Some(get_worker_metadata_response::Result::Error(WorkerError {
                error:
                    Some(Error::InternalError(WorkerExecutionError {
                        error: Some(worker_execution_error::Error::WorkerNotFound(_)),
                    })),
            })) => Ok(None),
            Some(get_worker_metadata_response::Result::Error(error)) => {
                Err(anyhow!("Failed to get worker metadata: {error:?}"))
            }
        }
    }

    async fn get_workers_metadata(
        &self,
        component_id: &ComponentId,
        filter: Option<WorkerFilter>,
        cursor: ScanCursor,
        count: u64,
        precise: bool,
    ) -> crate::Result<(Option<ScanCursor>, Vec<WorkerMetadata>)> {
        let component_id: golem_api_grpc::proto::golem::component::ComponentId =
            component_id.clone().into();
        let response = self
            .worker_service()
            .get_workers_metadata(GetWorkersMetadataRequest {
                component_id: Some(component_id),
                filter: filter.map(|f| f.into()),
                cursor: Some(cursor.into()),
                count,
                precise,
            })
            .await?;
        match response.result {
            None => Err(anyhow!("No response from get_workers_metadata")),
            Some(get_workers_metadata_response::Result::Success(
                GetWorkersMetadataSuccessResponse { workers, cursor },
            )) => Ok((
                cursor.map(|c| c.into()),
                workers.iter().map(to_worker_metadata).collect(),
            )),
            Some(get_workers_metadata_response::Result::Error(error)) => {
                Err(anyhow!("Failed to get workers metadata: {error:?}"))
            }
        }
    }

    async fn delete_worker(&self, worker_id: &WorkerId) -> crate::Result<()> {
        let _ = self
            .worker_service()
            .delete_worker(DeleteWorkerRequest {
                worker_id: Some(worker_id.clone().into()),
            })
            .await?;
        Ok(())
    }

    async fn invoke(
        &self,
        worker_id: &WorkerId,
        function_name: &str,
        params: Vec<Value>,
    ) -> crate::Result<Result<(), Error>> {
        let invoke_response = self
            .worker_service()
            .invoke(InvokeRequest {
                worker_id: Some(worker_id.clone().into()),
                idempotency_key: None,
                function: function_name.to_string(),
                invoke_parameters: Some(InvokeParameters {
                    params: params.into_iter().map(|v| v.into()).collect(),
                }),
                context: None,
            })
            .await?;

        match invoke_response.result {
            None => Err(anyhow!("No response from invoke_worker")),
            Some(invoke_response::Result::Success(_)) => Ok(Ok(())),
            Some(invoke_response::Result::Error(WorkerError { error: Some(error) })) => {
                Ok(Err(error))
            }
            Some(invoke_response::Result::Error(_)) => {
                Err(anyhow!("Empty error response from invoke_worker"))
            }
        }
    }

    async fn invoke_with_key(
        &self,
        worker_id: &WorkerId,
        idempotency_key: &IdempotencyKey,
        function_name: &str,
        params: Vec<Value>,
    ) -> crate::Result<Result<(), Error>> {
        let invoke_response = self
            .worker_service()
            .invoke(InvokeRequest {
                worker_id: Some(worker_id.clone().into()),
                idempotency_key: Some(idempotency_key.clone().into()),
                function: function_name.to_string(),
                invoke_parameters: Some(InvokeParameters {
                    params: params.into_iter().map(|v| v.into()).collect(),
                }),
                context: None,
            })
            .await?;

        match invoke_response.result {
            None => Err(anyhow!("No response from invoke_worker")),
            Some(invoke_response::Result::Success(_)) => Ok(Ok(())),
            Some(invoke_response::Result::Error(WorkerError { error: Some(error) })) => {
                Ok(Err(error))
            }
            Some(invoke_response::Result::Error(_)) => {
                Err(anyhow!("Empty error response from invoke_worker"))
            }
        }
    }

    async fn invoke_and_await(
        &self,
        worker_id: &WorkerId,
        function_name: &str,
        params: Vec<Value>,
    ) -> crate::Result<Result<Vec<Value>, Error>> {
        TestDsl::invoke_and_await_custom(self, worker_id, function_name, params).await
    }

    async fn invoke_and_await_with_key(
        &self,
        worker_id: &WorkerId,
        idempotency_key: &IdempotencyKey,
        function_name: &str,
        params: Vec<Value>,
    ) -> crate::Result<Result<Vec<Value>, Error>> {
        TestDsl::invoke_and_await_custom_with_key(
            self,
            worker_id,
            idempotency_key,
            function_name,
            params,
        )
        .await
    }

    async fn invoke_and_await_custom(
        &self,
        worker_id: &WorkerId,
        function_name: &str,
        params: Vec<Value>,
    ) -> crate::Result<Result<Vec<Value>, Error>> {
        let idempotency_key = IdempotencyKey::fresh();
        TestDsl::invoke_and_await_custom_with_key(
            self,
            worker_id,
            &idempotency_key,
            function_name,
            params,
        )
        .await
    }

    async fn invoke_and_await_custom_with_key(
        &self,
        worker_id: &WorkerId,
        idempotency_key: &IdempotencyKey,
        function_name: &str,
        params: Vec<Value>,
    ) -> crate::Result<Result<Vec<Value>, Error>> {
        let invoke_response = self
            .worker_service()
            .invoke_and_await(InvokeAndAwaitRequest {
                worker_id: Some(worker_id.clone().into()),
                idempotency_key: Some(idempotency_key.clone().into()),
                function: function_name.to_string(),
                invoke_parameters: Some(InvokeParameters {
                    params: params.into_iter().map(|v| v.into()).collect(),
                }),
                context: None,
            })
            .await?;

        match invoke_response.result {
            None => Err(anyhow!("No response from invoke_and_await")),
            Some(invoke_and_await_response::Result::Success(response)) => Ok(Ok(response
                .result
                .into_iter()
                .map(|v| v.try_into())
                .collect::<Result<Vec<Value>, String>>()
                .map_err(|err| anyhow!("Invocation result had unexpected format: {err}"))?)),
            Some(invoke_and_await_response::Result::Error(WorkerError { error: Some(error) })) => {
                Ok(Err(error))
            }
            Some(invoke_and_await_response::Result::Error(_)) => {
                Err(anyhow!("Empty error response from invoke_and_await"))
            }
        }
    }

    async fn capture_output(&self, worker_id: &WorkerId) -> UnboundedReceiver<LogEvent> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let cloned_service = self.worker_service().clone();
        let worker_id = worker_id.clone();
        tokio::spawn(async move {
            let mut response = cloned_service
                .connect_worker(ConnectWorkerRequest {
                    worker_id: Some(worker_id.clone().into()),
                })
                .await
                .expect("Failed to connect worker");

            while let Some(event) = response.message().await.expect("Failed to get message") {
                debug!("Received event: {:?}", event);
                tx.send(event).expect("Failed to send event");
            }

            debug!("Finished receiving events");
        });

        rx
    }

    async fn capture_output_forever(
        &self,
        worker_id: &WorkerId,
    ) -> (
        UnboundedReceiver<Option<LogEvent>>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let cloned_service = self.worker_service().clone();
        let worker_id = worker_id.clone();
        let (abort_tx, mut abort_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut abort = false;
            while !abort {
                let mut response = cloned_service
                    .connect_worker(ConnectWorkerRequest {
                        worker_id: Some(worker_id.clone().into()),
                    })
                    .await
                    .expect("Failed to connect worker");

                loop {
                    select! {
                        msg = response.message() => {
                            match msg {
                                Ok(Some(event)) =>  {
                                    debug!("Received event: {:?}", event);
                                    tx.send(Some(event)).expect("Failed to send event");
                                }
                                Ok(None) => {
                                    break;
                                }
                                Err(e) => {
                                    panic!("Failed to get message: {:?}", e);
                                }
                            }
                        }
                        _ = (&mut abort_rx) => {
                            abort = true;
                            break;
                        }
                    }
                }
            }

            tx.send(None).expect("Failed to send event");
            debug!("Finished receiving events");
        });

        (rx, abort_tx)
    }

    async fn capture_output_with_termination(
        &self,
        worker_id: &WorkerId,
    ) -> UnboundedReceiver<Option<LogEvent>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let cloned_service = self.worker_service().clone();
        let worker_id = worker_id.clone();
        tokio::spawn(async move {
            let mut response = cloned_service
                .connect_worker(ConnectWorkerRequest {
                    worker_id: Some(worker_id.clone().into()),
                })
                .await
                .expect("Failed to connect to worker");

            while let Some(event) = response.message().await.expect("Failed to get message") {
                debug!("Received event: {:?}", event);
                tx.send(Some(event)).expect("Failed to send event");
            }

            debug!("Finished receiving events");
            tx.send(None).expect("Failed to send termination event");
        });

        rx
    }

    async fn log_output(&self, worker_id: &WorkerId) {
        let cloned_service = self.worker_service().clone();
        let worker_id = worker_id.clone();
        tokio::spawn(async move {
            let mut response = cloned_service
                .connect_worker(ConnectWorkerRequest {
                    worker_id: Some(worker_id.clone().into()),
                })
                .await
                .expect("Failed to connect worker");

            while let Some(event) = response.message().await.expect("Failed to get message") {
                info!("Received event: {:?}", event);
            }
        });
    }

    async fn resume(&self, worker_id: &WorkerId) -> crate::Result<()> {
        let response = self
            .worker_service()
            .resume_worker(ResumeWorkerRequest {
                worker_id: Some(worker_id.clone().into()),
            })
            .await?;

        match response.result {
            None => Err(anyhow!("No response from connect_worker")),
            Some(resume_worker_response::Result::Success(_)) => Ok(()),
            Some(resume_worker_response::Result::Error(error)) => {
                Err(anyhow!("Failed to connect worker: {error:?}"))
            }
        }
    }

    async fn interrupt(&self, worker_id: &WorkerId) -> crate::Result<()> {
        let response = self
            .worker_service()
            .interrupt_worker(InterruptWorkerRequest {
                worker_id: Some(worker_id.clone().into()),
                recover_immediately: false,
            })
            .await?;

        match response {
            InterruptWorkerResponse {
                result: Some(interrupt_worker_response::Result::Success(_)),
            } => Ok(()),
            InterruptWorkerResponse {
                result: Some(interrupt_worker_response::Result::Error(error)),
            } => panic!("Failed to interrupt worker: {error:?}"),
            _ => panic!("Failed to interrupt worker: unknown error"),
        }
    }

    async fn simulated_crash(&self, worker_id: &WorkerId) -> crate::Result<()> {
        let response = self
            .worker_service()
            .interrupt_worker(InterruptWorkerRequest {
                worker_id: Some(worker_id.clone().into()),
                recover_immediately: true,
            })
            .await?;

        match response {
            InterruptWorkerResponse {
                result: Some(interrupt_worker_response::Result::Success(_)),
            } => Ok(()),
            InterruptWorkerResponse {
                result: Some(interrupt_worker_response::Result::Error(error)),
            } => Err(anyhow!("Failed to crash worker: {error:?}")),
            _ => Err(anyhow!("Failed to crash worker: unknown error")),
        }
    }

    async fn auto_update_worker(
        &self,
        worker_id: &WorkerId,
        target_version: ComponentVersion,
    ) -> crate::Result<()> {
        let response = self
            .worker_service()
            .update_worker(UpdateWorkerRequest {
                worker_id: Some(worker_id.clone().into()),
                target_version,
                mode: UpdateMode::Automatic.into(),
            })
            .await?;

        match response {
            UpdateWorkerResponse {
                result: Some(update_worker_response::Result::Success(_)),
            } => Ok(()),
            UpdateWorkerResponse {
                result: Some(update_worker_response::Result::Error(error)),
            } => Err(anyhow!("Failed to update worker: {error:?}")),
            _ => Err(anyhow!("Failed to update worker: unknown error")),
        }
    }

    async fn manual_update_worker(
        &self,
        worker_id: &WorkerId,
        target_version: ComponentVersion,
    ) -> crate::Result<()> {
        let response = self
            .worker_service()
            .update_worker(UpdateWorkerRequest {
                worker_id: Some(worker_id.clone().into()),
                target_version,
                mode: UpdateMode::Manual.into(),
            })
            .await?;

        match response {
            UpdateWorkerResponse {
                result: Some(update_worker_response::Result::Success(_)),
            } => Ok(()),
            UpdateWorkerResponse {
                result: Some(update_worker_response::Result::Error(error)),
            } => Err(anyhow!("Failed to update worker: {error:?}")),
            _ => Err(anyhow!("Failed to update worker: unknown error")),
        }
    }
}

pub fn stdout_event(s: &str) -> LogEvent {
    LogEvent {
        event: Some(log_event::Event::Stdout(StdOutLog {
            message: s.to_string(),
        })),
    }
}

pub fn stdout_event_starting_with(event: &LogEvent, s: &str) -> bool {
    if let LogEvent {
        event: Some(log_event::Event::Stdout(StdOutLog { message })),
    } = event
    {
        message.starts_with(s)
    } else {
        false
    }
}

pub fn stderr_event(s: &str) -> LogEvent {
    LogEvent {
        event: Some(log_event::Event::Stderr(StdErrLog {
            message: s.to_string(),
        })),
    }
}

pub fn log_event_to_string(event: &LogEvent) -> String {
    match &event.event {
        Some(log_event::Event::Stdout(stdout)) => stdout.message.clone(),
        Some(log_event::Event::Stderr(stderr)) => stderr.message.clone(),
        Some(log_event::Event::Log(log)) => log.message.clone(),
        _ => std::panic!("Unexpected event type"),
    }
}

pub async fn drain_connection(rx: UnboundedReceiver<Option<LogEvent>>) -> Vec<Option<LogEvent>> {
    let mut rx = rx;
    let mut events = vec![];
    rx.recv_many(&mut events, 100).await;

    if !events.contains(&None) {
        loop {
            match rx.recv().await {
                Some(Some(event)) => events.push(Some(event)),
                Some(None) => break,
                None => break,
            }
        }
    }
    events
}

pub async fn events_to_lines(rx: &mut UnboundedReceiver<LogEvent>) -> Vec<String> {
    let mut events = vec![];
    rx.recv_many(&mut events, 100).await;
    let full_output = events
        .iter()
        .map(log_event_to_string)
        .collect::<Vec<_>>()
        .join("");
    let lines = full_output
        .lines()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    lines
}

pub fn is_worker_execution_error(got: &Error, expected: &worker_execution_error::Error) -> bool {
    matches!(got, Error::InternalError(error) if error.error.as_ref() == Some(expected))
}

pub fn worker_error_message(error: &Error) -> String {
    match error {
        Error::BadRequest(errors) => errors.errors.join(", "),
        Error::Unauthorized(error) => error.error.clone(),
        Error::LimitExceeded(error) => error.error.clone(),
        Error::NotFound(error) => error.error.clone(),
        Error::AlreadyExists(error) => error.error.clone(),
        Error::InternalError(error) => match &error.error {
            None => "Internal error".to_string(),
            Some(error) => match error {
                worker_execution_error::Error::InvalidRequest(error) => error.details.clone(),
                worker_execution_error::Error::WorkerAlreadyExists(error) => {
                    format!("Worker already exists: {:?}", error.worker_id)
                }
                worker_execution_error::Error::WorkerCreationFailed(error) => format!(
                    "Worker creation failed: {:?}: {}",
                    error.worker_id, error.details
                ),
                worker_execution_error::Error::FailedToResumeWorker(error) => {
                    format!("Failed to resume worker: {:?}", error.worker_id)
                }
                worker_execution_error::Error::ComponentDownloadFailed(error) => format!(
                    "Failed to download component: {:?} version {}: {}",
                    error.component_id, error.component_version, error.reason
                ),
                worker_execution_error::Error::ComponentParseFailed(error) => format!(
                    "Failed to parse component: {:?} version {}: {}",
                    error.component_id, error.component_version, error.reason
                ),
                worker_execution_error::Error::GetLatestVersionOfComponentFailed(error) => format!(
                    "Failed to get latest version of component: {:?}: {}",
                    error.component_id, error.reason
                ),
                worker_execution_error::Error::PromiseNotFound(error) => {
                    format!("Promise not found: {:?}", error.promise_id)
                }
                worker_execution_error::Error::PromiseDropped(error) => {
                    format!("Promise dropped: {:?}", error.promise_id)
                }
                worker_execution_error::Error::PromiseAlreadyCompleted(error) => {
                    format!("Promise already completed: {:?}", error.promise_id)
                }
                worker_execution_error::Error::Interrupted(error) => {
                    if error.recover_immediately {
                        "Simulated crash".to_string()
                    } else {
                        "Interrupted via the Golem API".to_string()
                    }
                }
                worker_execution_error::Error::ParamTypeMismatch(_error) => {
                    "Parameter type mismatch".to_string()
                }
                worker_execution_error::Error::NoValueInMessage(_error) => {
                    "No value in message".to_string()
                }
                worker_execution_error::Error::ValueMismatch(error) => {
                    format!("Value mismatch: {}", error.details)
                }
                worker_execution_error::Error::UnexpectedOplogEntry(error) => format!(
                    "Unexpected oplog entry; Expected: {}, got: {}",
                    error.expected, error.got
                ),
                worker_execution_error::Error::RuntimeError(error) => {
                    format!("Runtime error: {}", error.details)
                }
                worker_execution_error::Error::InvalidShardId(error) => format!(
                    "Invalid shard id: {:?}; ids: {:?}",
                    error.shard_id, error.shard_ids
                ),
                worker_execution_error::Error::PreviousInvocationFailed(error) => {
                    format!("Previous invocation failed: {}", error.details)
                }
                worker_execution_error::Error::Unknown(error) => {
                    format!("Unknown error: {}", error.details)
                }
                worker_execution_error::Error::PreviousInvocationExited(_error) => {
                    "Previous invocation exited".to_string()
                }
                worker_execution_error::Error::InvalidAccount(_error) => {
                    "Invalid account id".to_string()
                }
                worker_execution_error::Error::WorkerNotFound(error) => {
                    format!("Worker not found: {:?}", error.worker_id)
                }
            },
        },
    }
}

pub fn to_worker_metadata(
    metadata: &golem_api_grpc::proto::golem::worker::WorkerMetadata,
) -> WorkerMetadata {
    WorkerMetadata {
        worker_id: metadata
            .worker_id
            .clone()
            .expect("no worker_id")
            .clone()
            .try_into()
            .expect("invalid worker_id"),
        args: metadata.args.clone(),
        env: metadata
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<Vec<_>>(),
        account_id: metadata
            .account_id
            .clone()
            .expect("no account_id")
            .clone()
            .into(),
        created_at: metadata
            .created_at
            .as_ref()
            .expect("no created_at")
            .clone()
            .into(),
        last_known_status: WorkerStatusRecord {
            oplog_idx: OplogIndex::default(),
            status: metadata.status.try_into().expect("invalid status"),
            overridden_retry_config: None, // not passed through gRPC
            deleted_regions: DeletedRegions::new(),
            pending_invocations: vec![],
            pending_updates: metadata
                .updates
                .iter()
                .filter_map(|u| match &u.update {
                    Some(Update::Pending(_)) => Some(TimestampedUpdateDescription {
                        timestamp: u
                            .timestamp
                            .as_ref()
                            .expect("no timestamp on update record")
                            .clone()
                            .into(),
                        oplog_index: OplogIndex::from_u64(0),
                        description: UpdateDescription::Automatic {
                            target_version: u.target_version,
                        },
                    }),
                    _ => None,
                })
                .collect(),
            failed_updates: metadata
                .updates
                .iter()
                .filter_map(|u| match &u.update {
                    Some(Update::Failed(failed_update)) => Some(FailedUpdateRecord {
                        timestamp: u
                            .timestamp
                            .as_ref()
                            .expect("no timestamp on update record")
                            .clone()
                            .into(),
                        target_version: u.target_version,
                        details: failed_update.details.clone(),
                    }),
                    _ => None,
                })
                .collect(),
            successful_updates: metadata
                .updates
                .iter()
                .filter_map(|u| match &u.update {
                    Some(Update::Successful(_)) => Some(SuccessfulUpdateRecord {
                        timestamp: u
                            .timestamp
                            .as_ref()
                            .expect("no timestamp on update record")
                            .clone()
                            .into(),
                        target_version: u.target_version,
                    }),
                    _ => None,
                })
                .collect(),
            invocation_results: HashMap::new(),
            current_idempotency_key: None,
            component_version: metadata.component_version,
            component_size: metadata.component_size,
            total_linear_memory_size: metadata.total_linear_memory_size,
            owned_resources: metadata
                .owned_resources
                .iter()
                .map(|(k, v)| {
                    (
                        WorkerResourceId(*k),
                        WorkerResourceDescription {
                            created_at: v
                                .created_at
                                .as_ref()
                                .expect("no timestamp on resource metadata")
                                .clone()
                                .into(),
                            indexed_resource_key: v.indexed.clone().map(|i| i.into()),
                        },
                    )
                })
                .collect(),
        },
        parent: None,
    }
}

fn dump_component_info(path: &Path) -> golem_common::model::component_metadata::ComponentMetadata {
    let data = std::fs::read(path).unwrap();

    let component_metadata: golem_common::model::component_metadata::ComponentMetadata =
        golem_common::model::component_metadata::ComponentMetadata::analyse_component(&data)
            .unwrap();

    let exports = &component_metadata.exports;
    let mems = &component_metadata.memories;

    info!("Exports of {path:?}: {exports:?}");
    info!("Linear memories of {path:?}: {mems:?}");

    component_metadata
}

async fn log_and_save_component_metadata(path: &Path) {
    let component_metadata: golem_common::model::component_metadata::ComponentMetadata =
        dump_component_info(path);

    let json_data = serde_json::to_string(&component_metadata).unwrap();

    // Write metadata to a path corresponding to component-id
    // This step is important for the following reason:
    // * this way it will perfectly simulate downloading the metadata from the component service even in the case of local-component-file tests.
    // * The test simulates what happens if you invoke an old wasm in component service (that has valid metadata but cannot be loaded anymore)
    // * The path is used to see if the metadata already exists for component analysis when it comes to local file
    // See ComponentServiceLocalFileSystem::get_component_metadata_file
    let component_name = path.file_name().unwrap().to_str().unwrap();
    let mut current_dir = Path::new("../target").to_path_buf();
    current_dir.push(component_name);
    current_dir.set_extension("json");
    tokio::fs::write(&current_dir, json_data).await.unwrap()
}

#[async_trait]
pub trait TestDslUnsafe {
    async fn store_component(&self, name: &str) -> ComponentId;
    async fn store_unique_component(&self, name: &str) -> ComponentId;
    async fn store_component_unverified(&self, name: &str) -> ComponentId;
    async fn update_component(&self, component_id: &ComponentId, name: &str) -> ComponentVersion;

    async fn start_worker(&self, component_id: &ComponentId, name: &str) -> WorkerId;
    async fn try_start_worker(
        &self,
        component_id: &ComponentId,
        name: &str,
    ) -> Result<WorkerId, Error>;
    async fn start_worker_with(
        &self,
        component_id: &ComponentId,
        name: &str,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> WorkerId;
    async fn try_start_worker_with(
        &self,
        component_id: &ComponentId,
        name: &str,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> Result<WorkerId, Error>;
    async fn get_worker_metadata(&self, worker_id: &WorkerId) -> Option<WorkerMetadata>;
    async fn get_workers_metadata(
        &self,
        component_id: &ComponentId,
        filter: Option<WorkerFilter>,
        cursor: ScanCursor,
        count: u64,
        precise: bool,
    ) -> (Option<ScanCursor>, Vec<WorkerMetadata>);
    async fn delete_worker(&self, worker_id: &WorkerId) -> ();

    async fn invoke(
        &self,
        worker_id: &WorkerId,
        function_name: &str,
        params: Vec<Value>,
    ) -> Result<(), Error>;
    async fn invoke_with_key(
        &self,
        worker_id: &WorkerId,
        idempotency_key: &IdempotencyKey,
        function_name: &str,
        params: Vec<Value>,
    ) -> Result<(), Error>;
    async fn invoke_and_await(
        &self,
        worker_id: &WorkerId,
        function_name: &str,
        params: Vec<Value>,
    ) -> Result<Vec<Value>, Error>;
    async fn invoke_and_await_with_key(
        &self,
        worker_id: &WorkerId,
        idempotency_key: &IdempotencyKey,
        function_name: &str,
        params: Vec<Value>,
    ) -> Result<Vec<Value>, Error>;
    async fn capture_output(&self, worker_id: &WorkerId) -> UnboundedReceiver<LogEvent>;
    async fn capture_output_forever(
        &self,
        worker_id: &WorkerId,
    ) -> (
        UnboundedReceiver<Option<LogEvent>>,
        tokio::sync::oneshot::Sender<()>,
    );
    async fn capture_output_with_termination(
        &self,
        worker_id: &WorkerId,
    ) -> UnboundedReceiver<Option<LogEvent>>;
    async fn log_output(&self, worker_id: &WorkerId);
    async fn resume(&self, worker_id: &WorkerId);
    async fn interrupt(&self, worker_id: &WorkerId);
    async fn simulated_crash(&self, worker_id: &WorkerId);
    async fn auto_update_worker(&self, worker_id: &WorkerId, target_version: ComponentVersion);
    async fn manual_update_worker(&self, worker_id: &WorkerId, target_version: ComponentVersion);
}

#[async_trait]
impl<T: TestDsl + Sync> TestDslUnsafe for T {
    async fn store_component(&self, name: &str) -> ComponentId {
        <T as TestDsl>::store_component(self, name).await
    }

    async fn store_unique_component(&self, name: &str) -> ComponentId {
        <T as TestDsl>::store_unique_component(self, name).await
    }

    async fn store_component_unverified(&self, name: &str) -> ComponentId {
        <T as TestDsl>::store_component_unverified(self, name).await
    }

    async fn update_component(&self, component_id: &ComponentId, name: &str) -> ComponentVersion {
        <T as TestDsl>::update_component(self, component_id, name).await
    }

    async fn start_worker(&self, component_id: &ComponentId, name: &str) -> WorkerId {
        <T as TestDsl>::start_worker(self, component_id, name)
            .await
            .expect("Failed to start worker")
    }

    async fn try_start_worker(
        &self,
        component_id: &ComponentId,
        name: &str,
    ) -> Result<WorkerId, Error> {
        <T as TestDsl>::try_start_worker(self, component_id, name)
            .await
            .expect("Failed to start worker")
    }

    async fn start_worker_with(
        &self,
        component_id: &ComponentId,
        name: &str,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> WorkerId {
        <T as TestDsl>::start_worker_with(self, component_id, name, args, env)
            .await
            .expect("Failed to start worker")
    }

    async fn try_start_worker_with(
        &self,
        component_id: &ComponentId,
        name: &str,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> Result<WorkerId, Error> {
        <T as TestDsl>::try_start_worker_with(self, component_id, name, args, env)
            .await
            .expect("Failed to start worker")
    }

    async fn get_worker_metadata(&self, worker_id: &WorkerId) -> Option<WorkerMetadata> {
        <T as TestDsl>::get_worker_metadata(self, worker_id)
            .await
            .expect("Failed to get worker metadata")
    }

    async fn get_workers_metadata(
        &self,
        component_id: &ComponentId,
        filter: Option<WorkerFilter>,
        cursor: ScanCursor,
        count: u64,
        precise: bool,
    ) -> (Option<ScanCursor>, Vec<WorkerMetadata>) {
        <T as TestDsl>::get_workers_metadata(self, component_id, filter, cursor, count, precise)
            .await
            .expect("Failed to get workers metadata")
    }

    async fn delete_worker(&self, worker_id: &WorkerId) -> () {
        <T as TestDsl>::delete_worker(self, worker_id)
            .await
            .expect("Failed to delete worker")
    }

    async fn invoke(
        &self,
        worker_id: &WorkerId,
        function_name: &str,
        params: Vec<Value>,
    ) -> Result<(), Error> {
        <T as TestDsl>::invoke(self, worker_id, function_name, params)
            .await
            .expect("Failed to invoke function")
    }

    async fn invoke_with_key(
        &self,
        worker_id: &WorkerId,
        idempotency_key: &IdempotencyKey,
        function_name: &str,
        params: Vec<Value>,
    ) -> Result<(), Error> {
        <T as TestDsl>::invoke_with_key(self, worker_id, idempotency_key, function_name, params)
            .await
            .expect("Failed to invoke function")
    }

    async fn invoke_and_await(
        &self,
        worker_id: &WorkerId,
        function_name: &str,
        params: Vec<Value>,
    ) -> Result<Vec<Value>, Error> {
        <T as TestDsl>::invoke_and_await(self, worker_id, function_name, params)
            .await
            .expect("Failed to invoke function")
    }

    async fn invoke_and_await_with_key(
        &self,
        worker_id: &WorkerId,
        idempotency_key: &IdempotencyKey,
        function_name: &str,
        params: Vec<Value>,
    ) -> Result<Vec<Value>, Error> {
        <T as TestDsl>::invoke_and_await_with_key(
            self,
            worker_id,
            idempotency_key,
            function_name,
            params,
        )
        .await
        .expect("Failed to invoke function")
    }

    async fn capture_output(&self, worker_id: &WorkerId) -> UnboundedReceiver<LogEvent> {
        <T as TestDsl>::capture_output(self, worker_id).await
    }

    async fn capture_output_forever(
        &self,
        worker_id: &WorkerId,
    ) -> (UnboundedReceiver<Option<LogEvent>>, Sender<()>) {
        <T as TestDsl>::capture_output_forever(self, worker_id).await
    }

    async fn capture_output_with_termination(
        &self,
        worker_id: &WorkerId,
    ) -> UnboundedReceiver<Option<LogEvent>> {
        <T as TestDsl>::capture_output_with_termination(self, worker_id).await
    }

    async fn log_output(&self, worker_id: &WorkerId) {
        <T as TestDsl>::log_output(self, worker_id).await
    }

    async fn resume(&self, worker_id: &WorkerId) {
        <T as TestDsl>::resume(self, worker_id)
            .await
            .expect("Failed to resume worker")
    }

    async fn interrupt(&self, worker_id: &WorkerId) {
        <T as TestDsl>::interrupt(self, worker_id)
            .await
            .expect("Failed to interrupt worker")
    }

    async fn simulated_crash(&self, worker_id: &WorkerId) {
        <T as TestDsl>::simulated_crash(self, worker_id)
            .await
            .expect("Failed to crash worker")
    }

    async fn auto_update_worker(&self, worker_id: &WorkerId, target_version: ComponentVersion) {
        <T as TestDsl>::auto_update_worker(self, worker_id, target_version)
            .await
            .expect("Failed to update worker")
    }

    async fn manual_update_worker(&self, worker_id: &WorkerId, target_version: ComponentVersion) {
        <T as TestDsl>::manual_update_worker(self, worker_id, target_version)
            .await
            .expect("Failed to update worker")
    }
}
