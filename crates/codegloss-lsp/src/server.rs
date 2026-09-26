//! The message loop: what arrives from the client, and what goes back.
//!
//! This is the half that [`crate::client`] is not. It owns the [`Backend`] and
//! turns a JSON-RPC message into one of its methods, which is all
//! `tower-lsp-server` used to do for this crate.
//!
//! The loop is synchronous, and every handler returns without waiting for
//! anything. That is not a simplification of the old design, it is the old
//! design's central rule made structural: AGENTS.md says no LSP request may
//! run the engine, and here there is no executor to run it on. A handler reads
//! the cache and pushes onto a queue; the pipeline's worker
//! ([`crate::translation`]) is the only thread that translates, and the only
//! one that ever blocks.
//!
//! [`Dispatcher::handle`] is a pure function of the message and the server's
//! state, which is what lets the protocol tests drive it directly instead of
//! standing up a transport.

use std::sync::Arc;

use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response, ResponseError};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::backend::Backend;
use crate::client::Client;

/// What the loop should do with a message it has handled.
#[derive(Debug)]
pub enum Action {
    /// Send this back to the client.
    Reply(Response),
    /// Nothing to send. Notifications and responses land here.
    Ignore,
    /// The client said `exit`. Stop reading.
    Exit,
}

/// Where the server is in its lifecycle.
///
/// The protocol gives the two ends of it different answers, and both are
/// observable: a client that asks before `initialize` has to be told to wait,
/// and one that asks after `shutdown` has to be told it is too late.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lifecycle {
    /// `initialize` has not arrived yet.
    Starting,
    /// Serving.
    Running,
    /// `shutdown` has been answered; only `exit` is still expected.
    ShuttingDown,
}

/// The [`Backend`] with the protocol's lifecycle rules around it.
#[derive(Debug)]
pub struct Dispatcher {
    backend: Backend,
    client: Arc<Client>,
    lifecycle: Lifecycle,
}

impl Dispatcher {
    #[must_use]
    pub fn new(backend: Backend, client: Arc<Client>) -> Self {
        Self {
            backend,
            client,
            lifecycle: Lifecycle::Starting,
        }
    }

    /// The server underneath, for the tests that look at what a handler did
    /// rather than at what it answered.
    #[must_use]
    pub fn backend(&self) -> &Backend {
        &self.backend
    }

    pub fn handle(&mut self, message: Message) -> Action {
        match message {
            Message::Request(request) => Action::Reply(self.respond(request)),
            Message::Notification(notification) => self.react(notification),
            // The answer to something this server asked. Whoever asked is
            // waiting on a channel, not on this loop.
            Message::Response(response) => {
                self.client.settle(response);
                Action::Ignore
            }
        }
    }

    fn respond(&mut self, request: Request) -> Response {
        let id = request.id.clone();
        let method = request.method.as_str();

        // The protocol's two closed doors. Both are errors the client is
        // expected to handle, and answering the request anyway would be worse
        // than either: before `initialize` this server has no document store
        // worth reading, and after `shutdown` its worker is on its way out.
        if self.lifecycle == Lifecycle::Starting && method != "initialize" {
            return error(
                id,
                ErrorCode::ServerNotInitialized,
                "server not initialized",
            );
        }
        if self.lifecycle == Lifecycle::ShuttingDown {
            return error(id, ErrorCode::InvalidRequest, "server is shutting down");
        }

        match method {
            "initialize" => {
                self.lifecycle = Lifecycle::Running;
                ok(id, self.backend.initialize())
            }
            "shutdown" => {
                self.lifecycle = Lifecycle::ShuttingDown;
                ok(id, ())
            }
            "textDocument/hover" => match parse(&request) {
                Ok(params) => ok(id, self.backend.hover(params)),
                Err(response) => response,
            },
            "textDocument/codeLens" => match parse(&request) {
                Ok(params) => ok(id, self.backend.code_lens(params)),
                Err(response) => response,
            },
            "workspace/executeCommand" => match parse(&request) {
                Ok(params) => {
                    self.backend.execute_command(params);
                    // Always `null`. Every lens carries a command because Zed
                    // draws none without one; running it does nothing.
                    ok(id, serde_json::Value::Null)
                }
                Err(response) => response,
            },
            _ => error(
                id,
                ErrorCode::MethodNotFound,
                &format!("unhandled request: {method}"),
            ),
        }
    }

    fn react(&mut self, notification: Notification) -> Action {
        let method = notification.method.as_str();

        // `exit` is answered whatever the lifecycle says: it is how a client
        // that never initialised, or one that gave up, closes the server.
        if method == "exit" {
            return Action::Exit;
        }
        if self.lifecycle == Lifecycle::Starting {
            // The specification says to drop these rather than complain.
            tracing::debug!(method, "a notification arrived before initialize");
            return Action::Ignore;
        }

        match method {
            "initialized" => self.backend.initialized(),
            "textDocument/didOpen" => match parse_notification(&notification) {
                Some(params) => self.backend.did_open(params),
                None => return Action::Ignore,
            },
            "textDocument/didChange" => match parse_notification(&notification) {
                Some(params) => self.backend.did_change(params),
                None => return Action::Ignore,
            },
            "textDocument/didClose" => match parse_notification(&notification) {
                Some(params) => self.backend.did_close(params),
                None => return Action::Ignore,
            },
            // Everything else, `$/cancelRequest` included. Nothing here is
            // worth cancelling: a handler answers from the cache and returns.
            _ => tracing::debug!(method, "ignoring a notification"),
        }
        Action::Ignore
    }
}

/// Reads messages until the client says `exit` or the connection closes.
pub fn serve(connection: &Connection, mut dispatcher: Dispatcher) {
    for message in &connection.receiver {
        match dispatcher.handle(message) {
            Action::Reply(response) => {
                if connection.sender.send(Message::Response(response)).is_err() {
                    tracing::warn!("the connection closed while answering");
                    break;
                }
            }
            Action::Ignore => {}
            Action::Exit => break,
        }
    }
    tracing::debug!("the message loop stopped");
}

fn parse<P: DeserializeOwned>(request: &Request) -> Result<P, Response> {
    serde_json::from_value(request.params.clone()).map_err(|error_| {
        error(
            request.id.clone(),
            ErrorCode::InvalidParams,
            &format!("{}: {error_}", request.method),
        )
    })
}

/// The same for a notification, which has nowhere to report a failure: the
/// protocol gives a notification no answer, so a malformed one is logged and
/// dropped.
fn parse_notification<P: DeserializeOwned>(notification: &Notification) -> Option<P> {
    match serde_json::from_value(notification.params.clone()) {
        Ok(params) => Some(params),
        Err(error) => {
            tracing::warn!(
                method = notification.method,
                %error,
                "a notification could not be read"
            );
            None
        }
    }
}

fn ok<R: Serialize>(id: lsp_server::RequestId, result: R) -> Response {
    Response::new_ok(id, result)
}

fn error(id: lsp_server::RequestId, code: ErrorCode, message: &str) -> Response {
    Response {
        id,
        response_result: Err(ResponseError {
            code: code as i32,
            message: message.to_owned(),
            data: None,
        }),
    }
}
