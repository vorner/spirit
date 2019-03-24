//! A tutorial how to use Spirit
//!
//! The module contains no actual code, only documentation explaining a step by step integration of
//! [`spirit`] into the application. So if you want to learn about how and why you might want to
//! use [`spirit`], read on. We'll be writing a full http service verifying if given numbers are
//! primes.
//!
//! # The Proof of Concept
//!
//! So, the boss asked us to write a service that given a natural number verifies if the number is
//! prime. It shall talk over http. So we start with a simple proof of concept that does just that,
//! with some help from the [`hyper`] crate and their [`example`][hyper-docs]
//!
//! ```rust,no_run
//! use std::iter;
//!
//! use hyper::{Body, Request, Response, Server};
//! use hyper::rt::Future;
//! use hyper::service::service_fn_ok;
//! use http::header;
//! use http::StatusCode;
//! use serde::Serialize;
//!
//! fn is_prime(n: u64) -> bool {
//!     iter::once(2)
//!         .chain((3..).step_by(2))
//!         .take_while(|d| *d * *d <= n)
//!         .all(|d| n % d != 0)
//! }
//!
//! #[derive(Serialize)]
//! #[serde(rename_all = "kebab-case")]
//! struct Output {
//!     input: u64,
//!     is_prime: bool,
//! }
//!
//! fn is_prime_svc(req: Request<Body>) -> Response<Body> {
//!     let mut path = req.uri().path();
//!     if path.starts_with("/") {
//!         path = &path[1..];
//!     }
//!     let input: u64 = path.parse().unwrap();
//!     let output = serde_json::to_string_pretty(&Output {
//!         input,
//!         is_prime: is_prime(input),
//!     }).unwrap();
//!
//!     Response::builder()
//!         .status(StatusCode::OK)
//!         .header(header::CONTENT_TYPE, "application/json")
//!         .body(Body::from(output))
//!         .expect("Failed to create proper response")
//! }
//!
//! fn main() {
//!     let addr = ([127, 0, 0, 1], 3456).into();
//!     let new_svc = || service_fn_ok(is_prime_svc);
//!
//!     let server = Server::bind(&addr)
//!         .serve(new_svc)
//!         .map_err(|e| eprintln!("server error: {}", e));
//!
//!     hyper::rt::run(server);
//! }
//!
//! #[cfg(test)]
//! mod tests {
//!     use super::*;
//!
//!     #[test]
//!     fn prime() {
//!         assert!(is_prime(2));
//!         assert!(is_prime(3));
//!         assert!(!is_prime(4));
//!         assert!(is_prime(5));
//!         assert!(!is_prime(12));
//!         assert!(is_prime(13));
//!     }
//! }
//! ```
//!
//! When we run it and query it, we actually get some answers:
//!
//! ```text
//! $ curl http://localhost:3456/42
//! {
//!   "input": 42,
//!   "is-prime": false
//! }
//! $ curl http://localhost:3456/13
//! {
//!   "input": 13,
//!   "is-prime": true
//! }
//! ```
//!
//! But this has a lot to be desired. Like, some error handling:
//!
//! ```text
//! $ curl http://localhost:3456/hello
//! curl: (52) Empty reply from server
//! ```
//!
//! ... and a panic in the service. So, let's fix that.
//!
//! ```rust,no_run
//! # use std::iter;
//! #
//! # use hyper::{Body, Request, Response, Server};
//! # use hyper::rt::Future;
//! # use hyper::service::service_fn_ok;
//! # use http::header;
//! # use http::StatusCode;
//! # use serde::Serialize;
//! #
//! # fn is_prime(n: u64) -> bool {
//! #     iter::once(2)
//! #         .chain((3..).step_by(2))
//! #         .take_while(|d| *d * *d <= n)
//! #         .all(|d| n % d != 0)
//! # }
//! #
//! # #[derive(Serialize)]
//! # #[serde(rename_all = "kebab-case")]
//! # struct Output {
//! #     input: u64,
//! #     is_prime: bool,
//! # }
//! #
//! fn is_prime_svc(req: Request<Body>) -> Result<Output, (StatusCode, &'static str)> {
//!     let mut path = req.uri().path();
//!     if path.starts_with("/") {
//!         path = &path[1..];
//!     }
//!     let input: u64 = path.parse()
//!         .map_err(|_| (StatusCode::NOT_FOUND, "Invalid number"))?;
//!     Ok(Output {
//!         input,
//!         is_prime: is_prime(input),
//!     })
//! }
//!
//! fn svc(req: Request<Body>) -> Response<Body> {
//!     match is_prime_svc(req) {
//!         Ok(output) => {
//!             let output = serde_json::to_string_pretty(&output)
//!                 .expect("Invalid json data is impossible here");
//!
//!             Response::builder()
//!                 .status(StatusCode::OK)
//!                 .header(header::CONTENT_TYPE, "application/json")
//!                 .body(Body::from(output))
//!                 .expect("Failed to create proper response")
//!         },
//!         Err((code, msg)) => {
//!             Response::builder()
//!                 .status(code)
//!                 .header(header::CONTENT_TYPE, "text/plain")
//!                 .body(Body::from(msg))
//!                 .expect("Failed to create proper response")
//!         }
//!     }
//! }
//! #
//! # fn main() {
//! #    let addr = ([127, 0, 0, 1], 3456).into();
//! #    let new_svc = || service_fn_ok(svc);
//! #
//! #    let server = Server::bind(&addr)
//! #        .serve(new_svc)
//! #        .map_err(|e| eprintln!("server error: {}", e));
//! #
//! #    hyper::rt::run(server);
//! # }
//! ```
//!
//! # Are we done yet?
//!
//!
//!
//! [`spirit`]: crate
//! [`hyper`]: https://crates.io/crates/hyper
//! [hyper-docs]: https://hyper.rs
