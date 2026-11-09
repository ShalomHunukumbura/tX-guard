use super::check;

/// (line, rule) for every finding in `source`.
fn found(source: &str) -> Vec<(usize, &'static str)> {
    check("app/views.py", source)
        .iter()
        .map(|f| (f.line, f.rule.code))
        .collect()
}

#[test]
fn a_task_queued_inside_atomic_is_found() {
    let src = "\
from django.db import transaction
from .tasks import send_welcome

def signup(form):
    with transaction.atomic():
        user = form.save()
        send_welcome.delay(user.id)
    send_welcome.delay(user.id)
";
    // The second call runs after the block, once the transaction has committed.
    assert_eq!(found(src), [(7, "TG001")]);
}

#[test]
fn the_message_says_where_the_transaction_opened() {
    let src = "\
from django.db import transaction
from .tasks import send_welcome

with transaction.atomic():
    send_welcome.apply_async(args=[1])
";
    let f = &check("x.py", src)[0];
    assert_eq!(
        f.message,
        "send_welcome.apply_async() queues a Celery task while the transaction opened on line 4 \
         is still open."
    );
    assert_eq!((f.line, f.col), (5, 5));
}

#[test]
fn on_commit_with_a_lambda_or_delay_on_commit_is_fine() {
    let src = "\
from django.db import transaction
from functools import partial
from .tasks import send_welcome

with transaction.atomic():
    transaction.on_commit(lambda: send_welcome.delay(1))
    transaction.on_commit(partial(send_welcome.delay, 1))
    send_welcome.delay_on_commit(1)
";
    assert_eq!(found(src), []);
}

#[test]
fn on_commit_given_the_result_of_delay_still_queues_now() {
    let src = "\
from django.db import transaction
from .tasks import send_welcome

with transaction.atomic():
    transaction.on_commit(send_welcome.delay(1))
";
    assert_eq!(found(src), [(5, "TG001")]);
}

#[test]
fn decorators_open_a_transaction_for_the_whole_function() {
    let src = "\
from django.db import transaction
from django.db.transaction import atomic as atom
from django.utils.decorators import method_decorator
from shop import tasks

@transaction.atomic
def a():
    tasks.ship.delay(1)

@atom(using='orders')
def b():
    tasks.ship.delay(1)

class View:
    @method_decorator(transaction.atomic)
    def post(self):
        tasks.ship.delay(1)

def not_atomic():
    tasks.ship.delay(1)
";
    assert_eq!(found(src), [(8, "TG001"), (12, "TG001"), (17, "TG001")]);
}

#[test]
fn imports_are_followed_whatever_they_are_called() {
    let src = "\
import django.db.transaction as tx
import django
from myapp.tasks import notify

with tx.atomic():
    notify.delay()
with django.db.transaction.atomic():
    notify.delay()
";
    assert_eq!(found(src), [(6, "TG001"), (8, "TG001")]);
}

#[test]
fn an_atomic_from_somewhere_else_is_not_a_transaction() {
    let src = "\
from mylib import atomic
from .tasks import notify

with atomic():
    notify.delay()
";
    assert_eq!(found(src), []);
}

#[test]
fn a_nested_function_runs_later_so_it_is_not_in_the_transaction() {
    let src = "\
from django.db import transaction
from .tasks import notify

with transaction.atomic():
    def later():
        notify.delay()
    transaction.on_commit(later)
";
    assert_eq!(found(src), []);
}

#[test]
fn tasks_defined_in_the_file_and_canvases_count() {
    let src = "\
from celery import shared_task, chain
from django.db import transaction

@shared_task
def resize(id):
    pass

@app.task(bind=True)
def crop(self, id):
    pass

with transaction.atomic():
    resize.delay(1)
    crop.s(1).delay()
    chain(resize.s(1), crop.s()).apply_async()
";
    assert_eq!(found(src), [(13, "TG001"), (14, "TG001"), (15, "TG001")]);
}

#[test]
fn delay_on_something_that_is_not_a_task_is_ignored() {
    let src = "\
from django.db import transaction

with transaction.atomic():
    self.delay(1)
    retry_policy.delay()
";
    assert_eq!(found(src), []);
}

#[test]
fn emails_inside_atomic_are_found() {
    let src = "\
from django.core.mail import send_mail, EmailMessage, get_connection
from django.db import transaction

with transaction.atomic():
    send_mail('Hi', 'body', 'a@x.lk', ['b@x.lk'])
    msg = EmailMessage('Hi', 'body', to=['b@x.lk'])
    msg.send()
    EmailMessage('Hi').send()
    conn = get_connection()
    conn.send_messages([msg])
    user.email_user('Hi', 'body')
";
    assert_eq!(
        found(src),
        [
            (5, "TG002"),
            (7, "TG002"),
            (8, "TG002"),
            (10, "TG002"),
            (11, "TG002")
        ]
    );
}

#[test]
fn a_name_given_something_else_is_no_longer_a_message() {
    let src = "\
from django.core.mail import EmailMessage
from django.db import transaction

with transaction.atomic():
    msg = EmailMessage('Hi')
    msg = build_sms()
    msg.send()
";
    assert_eq!(found(src), []);
}

#[test]
fn http_writes_are_errors_and_reads_are_warnings() {
    let src = "\
import requests
import httpx
from urllib.request import urlopen
from django.db import transaction

with transaction.atomic():
    requests.post('https://pay.lk/charge', json={})
    requests.get('https://rates.lk')
    httpx.delete('https://crm/x')
    requests.request('put', 'https://crm/x')
    requests.request(method, 'https://crm/x')
    urlopen('https://x')
    urlopen('https://x', b'data')
";
    assert_eq!(
        found(src),
        [
            (7, "TG003"),
            (8, "TG004"),
            (9, "TG003"),
            (10, "TG003"),
            (11, "TG004"),
            (12, "TG004"),
            (13, "TG003"),
        ]
    );
}

#[test]
fn http_clients_are_followed() {
    let src = "\
import requests
import httpx
from django.db import transaction

s = requests.Session()

def charge():
    s.post('https://pay.lk')

with transaction.atomic():
    with httpx.Client(timeout=5) as client:
        client.post('https://pay.lk')
    session = requests.Session()
    session.get('https://rates.lk')
";
    // `s` belongs to the module, not the transaction's scope, and charge() isn't called
    // here; following calls into helpers is milestone 2.
    assert_eq!(found(src), [(12, "TG003"), (14, "TG004")]);
}

#[test]
fn calls_in_the_with_line_itself_run_before_the_transaction() {
    let src = "\
import requests
from django.db import transaction

with transaction.atomic(using=requests.get('https://router').text):
    pass
";
    assert_eq!(found(src), []);
}

#[test]
fn any_item_of_a_with_can_open_the_transaction() {
    let src = "\
from django.db import transaction
from .tasks import notify

def f():
    with open('x') as fh, transaction.atomic():
        notify.delay()
";
    assert_eq!(found(src), [(6, "TG001")]);
}

#[test]
fn comments_and_strings_are_not_calls() {
    let src = "\
from django.db import transaction
from .tasks import notify

with transaction.atomic():
    # notify.delay() would be wrong here
    log('notify.delay()')
";
    assert_eq!(found(src), []);
}

#[test]
fn a_file_without_atomic_is_skipped_without_parsing() {
    assert_eq!(found("from .tasks import notify\nnotify.delay()\n"), []);
}
