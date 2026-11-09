//! Python: find side effects that run while a Django transaction is open.
//!
//! A transaction is open in the body of `with transaction.atomic():` and in the body of a
//! function decorated with `@transaction.atomic` (with or without arguments, or through
//! `method_decorator`). Imports are followed, so `from django.db.transaction import atomic`
//! and `import django.db.transaction as tx` both count.
//!
//! A nested `def` or `lambda` ends the region: its body runs later, when something calls it.
//! That is why `transaction.on_commit(lambda: task.delay(id))` is fine and needs no special
//! case, while `transaction.on_commit(task.delay(id))`, which queues the task straight away
//! and hands on_commit its result, is still found.

use std::collections::{HashMap, HashSet};

use tree_sitter::{Node, Parser};

use crate::finding::Finding;
use crate::rules::{
    EMAIL_BEFORE_COMMIT, HTTP_READ_IN_TRANSACTION, HTTP_WRITE_IN_TRANSACTION, TASK_BEFORE_COMMIT,
};
use crate::syntax::{named_children, text, unquote};

mod effects;

use effects::{Http, ATOMIC, HTTP_CLIENTS, MAIL_CONNECTION, MAIL_FUNCTIONS, MAIL_MESSAGES};

/// Names a file binds by importing.
#[derive(Default, Debug)]
struct Imports {
    /// `import requests as r` binds r -> requests.
    modules: HashMap<String, String>,
    /// `from requests import post as p` binds p -> requests.post. Relative imports keep
    /// their dots: `from .tasks import send` binds send -> .tasks.send.
    names: HashMap<String, String>,
}

impl Imports {
    fn binds(&self, name: &str) -> bool {
        self.modules.contains_key(name) || self.names.contains_key(name)
    }
}

/// What a local name was given, as far as side effects go.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Value {
    /// An `EmailMessage` or `EmailMultiAlternatives`; `.send()` sends it.
    Mail,
    /// `get_connection()`; `.send_messages()` sends through it.
    MailConnection,
    /// A `requests.Session` or `httpx.Client`.
    HttpClient,
}

/// Local names in the function being walked.
type Scope = HashMap<String, Value>;

struct Ctx<'a> {
    src: &'a [u8],
    path: &'a str,
    imports: Imports,
    /// Functions in this file decorated as Celery tasks.
    tasks: HashSet<String>,
    findings: Vec<Finding>,
}

pub fn check(path: &str, source: &str) -> Vec<Finding> {
    // Every Django transaction goes through `atomic`; a file without the word has none.
    if !source.contains(ATOMIC_WORD) {
        return Vec::new();
    }
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .expect("the Python grammar matches the tree-sitter version");
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let src = source.as_bytes();
    let root = tree.root_node();

    let mut ctx = Ctx {
        src,
        path,
        imports: Imports::default(),
        tasks: HashSet::new(),
        findings: Vec::new(),
    };
    collect(root, &mut ctx);
    visit(root, None, &mut Scope::new(), &mut ctx);
    ctx.findings
}

const ATOMIC_WORD: &str = "atomic";

/// First pass: imports anywhere in the file, and functions decorated as tasks.
fn collect(node: Node, ctx: &mut Ctx) {
    match node.kind() {
        "import_statement" | "import_from_statement" => collect_import(node, ctx),
        "decorated_definition" => {
            let task = decorators(node).any(|d| is_task_decorator(d, ctx.src));
            if let Some(name) = node
                .child_by_field_name("definition")
                .and_then(|d| d.child_by_field_name("name"))
            {
                if task {
                    ctx.tasks.insert(text(name, ctx.src).to_string());
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect(child, ctx);
    }
}

fn collect_import(node: Node, ctx: &mut Ctx) {
    let src = ctx.src;
    let imports = &mut ctx.imports;
    let mut cursor = node.walk();
    if node.kind() == "import_statement" {
        for name in node.children_by_field_name("name", &mut cursor) {
            match name.kind() {
                // `import django.db.transaction` binds `django`, and the full path resolves
                // through it.
                "dotted_name" => {
                    let full = text(name, src);
                    let first = full.split('.').next().unwrap_or(full);
                    imports.modules.insert(first.into(), first.into());
                }
                "aliased_import" => {
                    if let (Some(n), Some(a)) = (
                        name.child_by_field_name("name"),
                        name.child_by_field_name("alias"),
                    ) {
                        imports
                            .modules
                            .insert(text(a, src).into(), text(n, src).into());
                    }
                }
                _ => {}
            }
        }
        return;
    }
    let Some(module) = node.child_by_field_name("module_name") else {
        return;
    };
    let module = text(module, src);
    for name in node.children_by_field_name("name", &mut cursor) {
        let (local, real) = match name.kind() {
            "dotted_name" => (text(name, src), text(name, src)),
            "aliased_import" => match (
                name.child_by_field_name("name"),
                name.child_by_field_name("alias"),
            ) {
                (Some(n), Some(a)) => (text(a, src), text(n, src)),
                _ => continue,
            },
            _ => continue,
        };
        let joined = if module.ends_with('.') {
            format!("{module}{real}")
        } else {
            format!("{module}.{real}")
        };
        imports.names.insert(local.to_string(), joined);
    }
}

/// The dotted name an expression refers to, with imports resolved: `tx.atomic` after
/// `from django.db import transaction as tx` is `django.db.transaction.atomic`. Names that
/// weren't imported resolve to nothing.
fn resolve(expr: Node, src: &[u8], imports: &Imports) -> Option<String> {
    match expr.kind() {
        "identifier" => imports.names.get(text(expr, src)).cloned(),
        "attribute" => {
            let attr = text(expr.child_by_field_name("attribute")?, src);
            let object = expr.child_by_field_name("object")?;
            let base = if object.kind() == "identifier" {
                let name = text(object, src);
                imports
                    .names
                    .get(name)
                    .or_else(|| imports.modules.get(name))
                    .cloned()?
            } else {
                resolve(object, src, imports)?
            };
            Some(format!("{base}.{attr}"))
        }
        _ => None,
    }
}

/// The decorator expressions on a decorated definition, without the `@`.
fn decorators(node: Node) -> impl Iterator<Item = Node> {
    named_children(node)
        .into_iter()
        .filter(|c| c.kind() == "decorator")
        .filter_map(|d| d.named_child(0))
}

/// `@shared_task`, `@app.task(bind=True)`, `@celery.task`: anything whose last name is
/// `task` or `shared_task`. Projects name their Celery app many ways, so the name is enough.
fn is_task_decorator(expr: Node, src: &[u8]) -> bool {
    let expr = callee_of(expr);
    let name = text(expr, src);
    let last = name.rsplit('.').next().unwrap_or(name);
    last == "task" || last == "shared_task"
}

/// A decorator or with-item may be called or not: `@atomic` and `@atomic(using="x")`.
fn callee_of(expr: Node) -> Node {
    if expr.kind() == "call" {
        expr.child_by_field_name("function").unwrap_or(expr)
    } else {
        expr
    }
}

fn is_atomic(expr: Node, ctx: &Ctx) -> bool {
    let callee = callee_of(expr);
    if resolve(callee, ctx.src, &ctx.imports).as_deref() == Some(ATOMIC) {
        return true;
    }
    // `@method_decorator(transaction.atomic, name="dispatch")` wraps a method in atomic.
    if expr.kind() == "call"
        && resolve(callee, ctx.src, &ctx.imports).as_deref()
            == Some("django.utils.decorators.method_decorator")
    {
        return first_argument(expr).is_some_and(|a| is_atomic(a, ctx));
    }
    false
}

fn arguments(call: Node) -> Vec<Node> {
    call.child_by_field_name("arguments")
        .map(named_children)
        .unwrap_or_default()
}

fn first_argument(call: Node) -> Option<Node> {
    arguments(call)
        .into_iter()
        .find(|a| a.kind() != "keyword_argument")
}

fn keyword<'t>(call: Node<'t>, name: &str, src: &[u8]) -> Option<Node<'t>> {
    arguments(call).into_iter().find_map(|a| {
        (a.kind() == "keyword_argument"
            && a.child_by_field_name("name")
                .is_some_and(|n| text(n, src) == name))
        .then(|| a.child_by_field_name("value"))
        .flatten()
    })
}

/// Walk the tree. `open` is the line the enclosing transaction was opened on, if any.
fn visit<'t>(node: Node<'t>, open: Option<usize>, scope: &mut Scope, ctx: &mut Ctx) {
    match node.kind() {
        "decorated_definition" => {
            // Decorator arguments run when the function is defined, outside its body.
            let atomic = decorators(node).any(|d| is_atomic(d, ctx));
            for d in decorators(node) {
                visit(d, open, scope, ctx);
            }
            if let Some(def) = node.child_by_field_name("definition") {
                if def.kind() == "function_definition" {
                    let opened = atomic.then(|| def.start_position().row + 1);
                    visit_function(def, open, opened, ctx);
                } else {
                    visit(def, open, scope, ctx);
                }
            }
            return;
        }
        "function_definition" => {
            visit_function(node, open, None, ctx);
            return;
        }
        "lambda" => {
            // The body runs whenever the lambda is called, which on_commit does after the
            // commit. Defaults run now.
            if let Some(params) = node.child_by_field_name("parameters") {
                visit(params, open, scope, ctx);
            }
            if let Some(body) = node.child_by_field_name("body") {
                visit(body, None, &mut Scope::new(), ctx);
            }
            return;
        }
        "with_statement" => {
            // `async with` puts a keyword before the clause, so find it by kind.
            let items: Vec<Node> = named_children(node)
                .into_iter()
                .find(|c| c.kind() == "with_clause")
                .map(named_children)
                .unwrap_or_default();
            let mut opened = None;
            for item in &items {
                let Some(value) = item.child_by_field_name("value") else {
                    continue;
                };
                let (expr, alias) = if value.kind() == "as_pattern" {
                    (
                        value.named_child(0).unwrap_or(value),
                        value.child_by_field_name("alias"),
                    )
                } else {
                    (value, None)
                };
                visit(expr, open, scope, ctx);
                if is_atomic(expr, ctx) {
                    opened.get_or_insert(node.start_position().row + 1);
                }
                if let Some(kind) = value_of(expr, scope, ctx) {
                    if let Some(name) = alias.and_then(|a| a.named_child(0)) {
                        if name.kind() == "identifier" {
                            scope.insert(text(name, ctx.src).to_string(), kind);
                        }
                    }
                }
            }
            if let Some(body) = node.child_by_field_name("body") {
                visit(body, open.or(opened), scope, ctx);
            }
            return;
        }
        "assignment" => {
            if let (Some(left), Some(right)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ) {
                visit(right, open, scope, ctx);
                if left.kind() == "identifier" {
                    let name = text(left, ctx.src).to_string();
                    match value_of(right, scope, ctx) {
                        Some(kind) => scope.insert(name, kind),
                        None => scope.remove(&name),
                    };
                }
                return;
            }
        }
        "call" => {
            if let Some(line) = open {
                check_call(node, line, scope, ctx);
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit(child, open, scope, ctx);
    }
}

/// A function's body runs when it's called, so it has its own region and its own names.
/// Parameter defaults run where the function is defined.
fn visit_function(def: Node, outer: Option<usize>, opened: Option<usize>, ctx: &mut Ctx) {
    if let Some(params) = def.child_by_field_name("parameters") {
        visit(params, outer, &mut Scope::new(), ctx);
    }
    if let Some(body) = def.child_by_field_name("body") {
        visit(body, opened, &mut Scope::new(), ctx);
    }
}

/// What a value is, if it's something whose methods have side effects.
fn value_of(expr: Node, scope: &Scope, ctx: &Ctx) -> Option<Value> {
    match expr.kind() {
        "call" => {
            let callee = resolve(expr.child_by_field_name("function")?, ctx.src, &ctx.imports)?;
            if MAIL_MESSAGES.contains(&callee.as_str()) {
                Some(Value::Mail)
            } else if callee == MAIL_CONNECTION {
                Some(Value::MailConnection)
            } else if HTTP_CLIENTS.contains(&callee.as_str()) {
                Some(Value::HttpClient)
            } else {
                None
            }
        }
        "identifier" => scope.get(text(expr, ctx.src)).copied(),
        _ => None,
    }
}

fn check_call(call: Node, line: usize, scope: &Scope, ctx: &mut Ctx) {
    let Some(function) = call.child_by_field_name("function") else {
        return;
    };
    let shown = shown_callee(function, ctx.src);
    let at = |rule, what: String| Finding::at(ctx.path, call, rule, what);
    let still_open = format!("while the transaction opened on line {line} is still open");

    if let Some(name) = resolve(function, ctx.src, &ctx.imports) {
        if MAIL_FUNCTIONS.contains(&name.as_str()) {
            let f = at(
                EMAIL_BEFORE_COMMIT,
                format!("{shown}() sends an email {still_open}."),
            );
            ctx.findings.push(f);
            return;
        }
        if let Some(http) = effects::http_function(&name) {
            let method = method_of(http, call, ctx.src);
            let f = http_finding(call, &shown, method, &still_open, ctx);
            ctx.findings.push(f);
            return;
        }
    }

    if function.kind() != "attribute" {
        return;
    }
    let (Some(object), Some(attr)) = (
        function.child_by_field_name("object"),
        function.child_by_field_name("attribute"),
    ) else {
        return;
    };
    let receiver = value_of(object, scope, ctx);
    let finding = match (text(attr, ctx.src), receiver) {
        ("delay" | "apply_async", _) if is_task(object, ctx) => Some(at(
            TASK_BEFORE_COMMIT,
            format!("{shown}() queues a Celery task {still_open}."),
        )),
        ("send", Some(Value::Mail)) | ("send_messages", Some(Value::MailConnection)) => Some(at(
            EMAIL_BEFORE_COMMIT,
            format!("{shown}() sends an email {still_open}."),
        )),
        // Django's AbstractUser.email_user calls send_mail.
        ("email_user", _) => Some(at(
            EMAIL_BEFORE_COMMIT,
            format!("{shown}() sends an email {still_open}."),
        )),
        (method, Some(Value::HttpClient)) => effects::http_method(method).map(|http| {
            let m = method_of(http, call, ctx.src);
            http_finding(call, &shown, m, &still_open, ctx)
        }),
        _ => None,
    };
    if let Some(f) = finding {
        ctx.findings.push(f);
    }
}

/// The HTTP method a call uses, when the code says.
fn method_of(http: Http, call: Node, src: &[u8]) -> Option<String> {
    match http {
        Http::Method(m) => Some(m.to_string()),
        // `requests.request("POST", url)`: the method is the first argument.
        Http::Generic => first_argument(call)
            .or_else(|| keyword(call, "method", src))
            .filter(|a| a.kind() == "string")
            .map(|a| unquote(text(a, src)).to_ascii_uppercase()),
        // urlopen sends a POST when it's given data.
        Http::Urlopen => {
            let data = arguments(call)
                .into_iter()
                .filter(|a| a.kind() != "keyword_argument")
                .nth(1)
                .or_else(|| keyword(call, "data", src));
            Some(if data.is_some_and(|d| text(d, src) != "None") {
                "POST".into()
            } else {
                "GET".into()
            })
        }
    }
}

fn http_finding(
    call: Node,
    shown: &str,
    method: Option<String>,
    still_open: &str,
    ctx: &Ctx,
) -> Finding {
    match method.as_deref() {
        Some("GET" | "HEAD" | "OPTIONS") => Finding::at(
            ctx.path,
            call,
            HTTP_READ_IN_TRANSACTION,
            format!("{shown}() waits on another service {still_open}."),
        ),
        Some(m) => Finding::at(
            ctx.path,
            call,
            HTTP_WRITE_IN_TRANSACTION,
            format!("{shown}() sends a {m} request {still_open}."),
        ),
        // A method from a variable: say so, and count it as the milder case.
        None => Finding::at(
            ctx.path,
            call,
            HTTP_READ_IN_TRANSACTION,
            format!(
                "{shown}() makes a request {still_open}. Its method isn't written out; if it \
                 changes something, a rollback can't undo it."
            ),
        ),
    }
}

/// Whether `.delay()` or `.apply_async()` is called on a Celery task: a name imported from
/// another module (tasks usually live in tasks.py), a function decorated as a task in this
/// file, or a signature or canvas built from one, like `send.s(1)` or `chain(a.s(), b.s())`.
/// A method on `self` or a local object is someone else's `delay`.
fn is_task(object: Node, ctx: &Ctx) -> bool {
    let src = ctx.src;
    match object.kind() {
        "identifier" => {
            let name = text(object, src);
            ctx.tasks.contains(name) || ctx.imports.names.contains_key(name)
        }
        "attribute" => {
            let mut head = object;
            while head.kind() == "attribute" {
                match head.child_by_field_name("object") {
                    Some(o) => head = o,
                    None => return false,
                }
            }
            head.kind() == "identifier" && ctx.imports.binds(text(head, src))
        }
        "call" => {
            let Some(function) = object.child_by_field_name("function") else {
                return false;
            };
            if let Some(name) = resolve(function, src, &ctx.imports) {
                if effects::CELERY_CANVAS.contains(&name.as_str()) {
                    return true;
                }
            }
            function.kind() == "attribute"
                && function
                    .child_by_field_name("attribute")
                    .is_some_and(|a| matches!(text(a, src), "s" | "si" | "signature"))
                && function
                    .child_by_field_name("object")
                    .is_some_and(|o| is_task(o, ctx))
        }
        _ => false,
    }
}

/// The callee as written, on one line and short enough for a message.
fn shown_callee(function: Node, src: &[u8]) -> String {
    let flat: String = text(function, src)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("");
    if flat.chars().count() > 60 {
        let tail: String = flat
            .chars()
            .rev()
            .take(57)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("...{tail}")
    } else {
        flat
    }
}

#[cfg(test)]
mod tests;
