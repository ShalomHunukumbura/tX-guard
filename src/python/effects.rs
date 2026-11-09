//! What tx-guard knows about Python libraries: which calls open a transaction and which
//! calls reach outside the database. Names are fully qualified, after imports are resolved.

/// Django's transaction, as a context manager or a decorator.
pub const ATOMIC: &str = "django.db.transaction.atomic";

/// Functions that send mail when called.
pub const MAIL_FUNCTIONS: &[&str] = &[
    "django.core.mail.send_mail",
    "django.core.mail.send_mass_mail",
    "django.core.mail.mail_admins",
    "django.core.mail.mail_managers",
];

/// Message classes whose `.send()` sends.
pub const MAIL_MESSAGES: &[&str] = &[
    "django.core.mail.EmailMessage",
    "django.core.mail.EmailMultiAlternatives",
    "django.core.mail.message.EmailMessage",
    "django.core.mail.message.EmailMultiAlternatives",
];

/// A mail connection, whose `.send_messages()` sends.
pub const MAIL_CONNECTION: &str = "django.core.mail.get_connection";

/// Clients whose request methods make HTTP calls.
pub const HTTP_CLIENTS: &[&str] = &[
    "requests.Session",
    "requests.session",
    "requests.sessions.Session",
    "httpx.Client",
    "httpx.AsyncClient",
];

/// Celery canvas builders; `.delay()` or `.apply_async()` on what they return queues tasks.
pub const CELERY_CANVAS: &[&str] = &[
    "celery.chain",
    "celery.group",
    "celery.chord",
    "celery.signature",
];

/// How a call picks its HTTP method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Http {
    /// Fixed by the function's name, like `requests.post`.
    Method(&'static str),
    /// Given as an argument, like `requests.request("POST", url)`.
    Generic,
    /// `urllib.request.urlopen`: POST when given data, else GET.
    Urlopen,
}

/// A module-level function that makes an HTTP request.
pub fn http_function(name: &str) -> Option<Http> {
    if name == "urllib.request.urlopen" {
        return Some(Http::Urlopen);
    }
    let (module, function) = name.rsplit_once('.')?;
    match module {
        "requests" | "httpx" => http_method(function),
        _ => None,
    }
}

/// A client method that makes an HTTP request, shared by requests and httpx.
pub fn http_method(name: &str) -> Option<Http> {
    Some(match name {
        "get" => Http::Method("GET"),
        "head" => Http::Method("HEAD"),
        "options" => Http::Method("OPTIONS"),
        "post" => Http::Method("POST"),
        "put" => Http::Method("PUT"),
        "patch" => Http::Method("PATCH"),
        "delete" => Http::Method("DELETE"),
        "request" | "stream" => Http::Generic,
        _ => return None,
    })
}
