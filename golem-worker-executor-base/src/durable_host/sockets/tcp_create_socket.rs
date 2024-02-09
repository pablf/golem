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

use async_trait::async_trait;
use wasmtime::component::Resource;

use crate::durable_host::DurableWorkerCtx;
use crate::metrics::wasm::record_host_function_call;
use crate::workerctx::WorkerCtx;
use wasmtime_wasi::preview2::bindings::wasi::sockets::tcp_create_socket::{
    Host, IpAddressFamily, TcpSocket,
};
use wasmtime_wasi::preview2::SocketError;

#[async_trait]
impl<Ctx: WorkerCtx> Host for DurableWorkerCtx<Ctx> {
    fn create_tcp_socket(
        &mut self,
        address_family: IpAddressFamily,
    ) -> Result<Resource<TcpSocket>, SocketError> {
        record_host_function_call("sockets::tcp_create_socket", "create_tcp_socket");
        Host::create_tcp_socket(&mut self.as_wasi_view(), address_family)
    }
}
