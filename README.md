# tx-guard

Find the emails, background jobs and API calls your code runs while a database transaction is
still open.

```python
@transaction.atomic
def place_order(cart, user):
    order = cart.to_order()
    requests.post("https://pay.example.lk/charge", json={"amount": order.total})
    send_mail("Order placed", f"Order {order.id}", "shop@example.lk", [user.email])
    ship_order.delay(order.id)
    return order
```

Each of those three calls is a bug waiting for a busy day:

- **`ship_order.delay()`** sends the task to the broker at once. A worker can start it before
  the commit, look up the order, and find nothing.
- **`send_mail()`** can't be rolled back. If anything after it fails, the customer has an
  email for an order that doesn't exist.
- **`requests.post()`** charges the card, and a rollback won't refund it. While it waits on
  the payment API, the transaction holds its row locks and a database connection.

Tests rarely catch any of this. With `CELERY_TASK_ALWAYS_EAGER` the task runs inline, inside
the same transaction, and the mail and HTTP calls are mocked. In review, the code looks fine.

```text
$ tx-guard shop/
shop/orders.py:11:5  TG003 error  requests.post() sends a POST request while the transaction opened on line 9 is still open.
    fix: Make the call after the commit with transaction.on_commit, or before the transaction and save its result inside it. ...
shop/orders.py:12:5  TG002 error  send_mail() sends an email while the transaction opened on line 9 is still open.
    fix: Send it after the commit with transaction.on_commit(lambda: send_mail(...)), or queue a task on commit that sends it. ...
shop/orders.py:13:5  TG001 error  ship_order.delay() queues a Celery task while the transaction opened on line 9 is still open.
    fix: Queue it after the commit: transaction.on_commit(lambda: task.delay(obj.id)), or task.delay_on_commit(obj.id) on Celery 5.4 or later.
Found 3 errors and 0 warnings in 1 file.
```

## What it checks

A transaction is open inside `with transaction.atomic():` and in any function decorated with
`@transaction.atomic`, `@atomic(using=...)` or `@method_decorator(transaction.atomic)`. Imports
are followed, so aliases work.

| Code | Name | Severity | Finds |
|------|------|----------|-------|
| TG001 | task-before-commit | error | Celery `.delay()` and `.apply_async()`, on tasks, signatures and canvases |
| TG002 | email-before-commit | error | `send_mail`, `send_mass_mail`, `mail_admins`, `mail_managers`, `EmailMessage.send()`, `send_messages()`, `user.email_user()` |
| TG003 | http-write-in-transaction | error | POST, PUT, PATCH and DELETE with `requests`, `httpx` (including sessions and clients) and `urlopen` with data |
| TG004 | http-read-in-transaction | warning | GET, HEAD and OPTIONS, and requests whose method is a variable |

What it leaves alone:

- `transaction.on_commit(lambda: task.delay(id))` and `task.delay_on_commit(id)`. A nested
  `def` or `lambda` runs later, so its body isn't in the transaction.
- `transaction.on_commit(task.delay(id))` *is* reported. It queues the task straight away and
  passes on_commit the result.
- `.delay()` on `self` or on a local object, which is someone else's `delay`.
- Calls in the `with` line itself, which run before the transaction opens.

`tx-guard --explain TG001` shows why a rule matters and how to fix it. `tx-guard --list-rules`
lists them all.

## Usage

```text
tx-guard [PATHS]...            # defaults to the current directory, skipping .gitignored files
  --format text|github|json    # github prints annotations for pull requests
  --strict                     # fail on warnings too
  --ignore TG004               # skip rules, comma-separated
  --no-ignore                  # check gitignored files as well
```

Exit codes: `0` clean, `1` findings, `2` usage error.

## Status

Milestone 1 of 6. Coming next: SQLAlchemy, your project's own wrappers around `atomic()`,
following calls into helper functions, queue publishes, JS/TS and Go, ignore comments and a
baseline, and prebuilt binaries with a GitHub Action.

## Licence

MIT
