//! The server-to-client half of the connection.
//!
//! Most of LSP is the client asking and the server answering, and that half is
//! [`crate::server`]. This is the other one: `window/logMessage` on the way up,
//! and the two `workspace/*/refresh` requests the translation pipeline sends
//! when a batch of glosses lands.
//!
//! A request needs its answer routed back to whoever sent it, and whoever sent
//! it is not the thread reading the connection: the pipeline runs on a worker
//! of its own ([`crate::translation`]). So a caller registers a one-shot
//! channel under the request's id, and the read loop hands the response to
//! [`Client::settle`], which finds the channel and completes the call.
//!
//! IMPORTANT: the read loop must never block on the worker, or an unanswered
//! refresh would stop the server from reading anything at all. It does not:
//! [`Client::settle`] takes a lock, sends on an unbounded channel and returns.
//! The waiting happens on the worker, under a timeout, which is what keeps a
//! client that accepts a refresh and never answers from stopping every future
//! translation.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use crossbeam_channel::{RecvTimeoutError, Sender};
use lsp_server::{Message, Notification, Request, RequestId, Response};
use serde::Serialize;

use crate::ls_types::{LogMessageParams, MessageType};

/// How a request to the client ended.
///
/// Every outcome but [`Outcome::Accepted`] is ordinary: not every client
/// implements the refresh requests, and one that does not says so per request.
#[derive(Debug)]
pub enum Outcome {
    /// The client answered, and the answer was not an error.
    Accepted,
    /// The client answered with an error. Its message is carried so the log
    /// can say which.
    Refused(String),
    /// The client took the request and said nothing for as long as it was
    /// given.
    TimedOut,
    /// The connection is gone. Nothing more will be answered.
    Disconnected,
}

/// The handle anything that wants to talk to the client holds.
///
/// Cloneable by sharing: the pipeline and the request handlers hold the same
/// one, because the outgoing ids have to come from a single counter.
#[derive(Debug)]
pub struct Client {
    sender: Sender<Message>,
    outgoing: Mutex<Outgoing>,
}

#[derive(Debug, Default)]
struct Outgoing {
    next_id: i32,
    /// Requests sent and not yet answered, each with the channel its caller
    /// is waiting on.
    waiting: HashMap<RequestId, Sender<Response>>,
}

impl Client {
    #[must_use]
    pub fn new(sender: Sender<Message>) -> Self {
        Self {
            sender,
            outgoing: Mutex::new(Outgoing::default()),
        }
    }

    /// Sends a notification. Nothing comes back, by definition.
    pub fn notify<P: Serialize>(&self, method: &str, params: P) {
        let notification = Notification::new(method.to_owned(), params);
        // A closed connection means the server is on its way out. There is
        // nobody left to tell.
        let _ = self.sender.send(Message::Notification(notification));
    }

    /// Tells the client something worth showing in its log pane.
    pub fn log_message(&self, message_type: MessageType, message: &str) {
        self.notify(
            "window/logMessage",
            LogMessageParams {
                message_type,
                message: message.to_owned(),
            },
        );
    }

    /// Asks the client to refetch the inlay hints it is showing.
    pub fn inlay_hint_refresh(&self, patience: Duration) -> Outcome {
        self.request("workspace/inlayHint/refresh", patience)
    }

    /// Asks the client to refetch the code lenses it is showing.
    pub fn code_lens_refresh(&self, patience: Duration) -> Outcome {
        self.request("workspace/codeLens/refresh", patience)
    }

    /// Sends a request with no parameters and waits up to `patience` for the
    /// answer.
    ///
    /// Blocking is deliberate and safe here: the only caller is the pipeline's
    /// own worker, and the thread that produces the answer is a different one.
    fn request(&self, method: &str, patience: Duration) -> Outcome {
        let (answered, answer) = crossbeam_channel::bounded(1);

        let request = {
            let Ok(mut outgoing) = self.outgoing.lock() else {
                // A panic in another thread poisoned the lock. The ids are
                // the only state behind it, so nothing is unsafe to use, but
                // something has already gone wrong and this is not the place
                // to decide what.
                return Outcome::Disconnected;
            };
            let id = RequestId::from(outgoing.next_id);
            outgoing.next_id += 1;
            outgoing.waiting.insert(id.clone(), answered);
            Request::new(id, method.to_owned(), serde_json::Value::Null)
        };
        let id = request.id.clone();

        if self.sender.send(Message::Request(request)).is_err() {
            self.forget(&id);
            return Outcome::Disconnected;
        }

        match answer.recv_timeout(patience) {
            Ok(response) => match response.response_result {
                Ok(_) => Outcome::Accepted,
                Err(error) => Outcome::Refused(error.message),
            },
            Err(RecvTimeoutError::Timeout) => {
                // IMPORTANT: dropped from the map, or a client that answers
                // late leaves an entry behind for every refresh it ignored.
                self.forget(&id);
                Outcome::TimedOut
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.forget(&id);
                Outcome::Disconnected
            }
        }
    }

    /// Hands a response from the client to whoever is waiting for it.
    ///
    /// Called by the read loop, and never blocking. A response to an id nobody
    /// is waiting on is dropped: the caller gave up on it, or the client
    /// invented it.
    pub fn settle(&self, response: Response) {
        let Ok(mut outgoing) = self.outgoing.lock() else {
            return;
        };
        let Some(waiting) = outgoing.waiting.remove(&response.id) else {
            tracing::debug!(id = %response.id, "a response arrived for nothing that is waiting");
            return;
        };
        // The receiver is a bounded(1) nobody else sends on, so this cannot
        // block; it fails only if the caller timed out between the lookup and
        // here, which is the same as not being waited on.
        let _ = waiting.send(response);
    }

    fn forget(&self, id: &RequestId) {
        if let Ok(mut outgoing) = self.outgoing.lock() {
            outgoing.waiting.remove(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::*;

    const PATIENCE: Duration = Duration::from_secs(5);

    fn client() -> (Arc<Client>, crossbeam_channel::Receiver<Message>) {
        let (sender, sent) = crossbeam_channel::unbounded();
        (Arc::new(Client::new(sender)), sent)
    }

    fn sent_request(sent: &crossbeam_channel::Receiver<Message>) -> Request {
        match sent.recv().expect("the client sent something") {
            Message::Request(request) => request,
            other => panic!("expected a request, got {other:?}"),
        }
    }

    #[test]
    fn a_notification_carries_its_parameters_and_expects_no_answer() {
        let (client, sent) = client();

        client.log_message(MessageType::INFO, "started");

        match sent.recv().expect("the client sent something") {
            Message::Notification(notification) => {
                assert_eq!(notification.method, "window/logMessage");
                assert_eq!(
                    notification.params,
                    serde_json::json!({ "type": 3, "message": "started" })
                );
            }
            other => panic!("expected a notification, got {other:?}"),
        }
    }

    /// The round trip the pipeline depends on: the worker blocks, the read
    /// loop settles the answer, the worker wakes.
    #[test]
    fn a_request_is_answered_through_settle() {
        let (client, sent) = client();
        let asking = Arc::clone(&client);
        let worker = thread::spawn(move || asking.code_lens_refresh(PATIENCE));

        let request = sent_request(&sent);
        assert_eq!(request.method, "workspace/codeLens/refresh");
        // No parameters at all, rather than an explicit null.
        assert!(request.params.is_null());
        client.settle(Response::new_ok(request.id, ()));

        assert!(matches!(
            worker.join().expect("the worker finished"),
            Outcome::Accepted
        ));
    }

    /// A client that does not implement a refresh says so per request, which
    /// is not worth bothering the user about - but the caller has to be able
    /// to tell it apart from an answer.
    #[test]
    fn an_error_answer_is_reported_as_a_refusal() {
        let (client, sent) = client();
        let asking = Arc::clone(&client);
        let worker = thread::spawn(move || asking.inlay_hint_refresh(PATIENCE));

        let request = sent_request(&sent);
        client.settle(Response::new_err(
            request.id,
            lsp_server::ErrorCode::MethodNotFound as i32,
            "unhandled method".to_owned(),
        ));

        match worker.join().expect("the worker finished") {
            Outcome::Refused(message) => assert_eq!(message, "unhandled method"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// A client that never answers must not keep the worker: the wait is
    /// bounded, and the entry it registered goes away with it.
    #[test]
    fn an_unanswered_request_times_out_and_leaves_nothing_behind() {
        let (client, sent) = client();
        let asking = Arc::clone(&client);
        let worker = thread::spawn(move || asking.code_lens_refresh(Duration::from_millis(50)));

        let request = sent_request(&sent);
        assert!(matches!(
            worker.join().expect("the worker finished"),
            Outcome::TimedOut
        ));

        assert!(
            client.outgoing.lock().unwrap().waiting.is_empty(),
            "a timed-out request stayed registered"
        );
        // And a late answer to it is dropped rather than panicking.
        client.settle(Response::new_ok(request.id, ()));
    }

    /// Every outgoing request needs an id of its own, or two refreshes in
    /// flight would settle each other.
    #[test]
    fn outgoing_requests_get_distinct_ids() {
        let (client, sent) = client();
        let first = Arc::clone(&client);
        let first = thread::spawn(move || first.code_lens_refresh(Duration::from_millis(50)));
        let one = sent_request(&sent).id;
        first.join().expect("the first finished");

        let second = Arc::clone(&client);
        let second = thread::spawn(move || second.code_lens_refresh(Duration::from_millis(50)));
        let two = sent_request(&sent).id;
        second.join().expect("the second finished");

        assert_ne!(one, two);
    }

    /// With the read loop gone there is nothing that could ever answer, and
    /// the caller has to learn that rather than wait out its patience.
    #[test]
    fn a_closed_connection_is_reported_at_once() {
        let (sender, sent) = crossbeam_channel::unbounded();
        let client = Client::new(sender);
        drop(sent);

        assert!(matches!(
            client.code_lens_refresh(Duration::from_secs(30)),
            Outcome::Disconnected
        ));
    }
}
