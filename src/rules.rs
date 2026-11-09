//! The rule catalog. Codes are stable: once released, a code keeps its meaning.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    /// Reported, never fails the run.
    Note,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        }
    }
}

/// The language a fix is written for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Any,
    Python,
}

impl Lang {
    fn of(path: &str) -> Lang {
        match path.rsplit('.').next().unwrap_or("") {
            "py" => Lang::Python,
            _ => Lang::Any,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Lang::Any => "",
            Lang::Python => "Python",
        }
    }
}

#[derive(Debug)]
pub struct Rule {
    pub code: &'static str,
    pub name: &'static str,
    pub severity: Severity,
    pub summary: &'static str,
    pub why: &'static str,
    /// How to fix it, per language. `--explain` shows them all; a finding shows its own.
    pub fix: &'static [(Lang, &'static str)],
}

pub const TASK_BEFORE_COMMIT: &str = "TG001";
pub const EMAIL_BEFORE_COMMIT: &str = "TG002";
pub const HTTP_WRITE_IN_TRANSACTION: &str = "TG003";
pub const HTTP_READ_IN_TRANSACTION: &str = "TG004";

pub const RULES: &[Rule] = &[
    Rule {
        code: TASK_BEFORE_COMMIT,
        name: "task-before-commit",
        severity: Severity::Error,
        summary: "A background task is queued while the transaction is still open, so a worker \
                  can run it before the commit, or after a rollback.",
        why: "Queuing a task sends a message to the broker at once; it doesn't wait for the \
              transaction. A worker can pick the task up within milliseconds, before the \
              commit, and the row it was given an id for isn't visible yet: the task fails \
              with DoesNotExist, or worse, reads the old values and carries on. If the \
              transaction rolls back, the task still runs, for a row that never existed. \
              Tests rarely catch it: with CELERY_TASK_ALWAYS_EAGER the task runs inline, \
              inside the same transaction, where the row is visible.",
        fix: &[(
            Lang::Python,
            "Queue it after the commit: transaction.on_commit(lambda: task.delay(obj.id)), or \
             task.delay_on_commit(obj.id) on Celery 5.4 or later.",
        )],
    },
    Rule {
        code: EMAIL_BEFORE_COMMIT,
        name: "email-before-commit",
        severity: Severity::Error,
        summary: "An email is sent while the transaction is still open, and a rollback can't \
                  unsend it.",
        why: "send_mail talks to the mail server while the transaction is open. If anything \
              later in the transaction fails, the database rolls back, but the email has \
              already gone: a welcome email for an account that was never created, or an \
              order confirmation for an order that isn't there. Sending also holds the \
              transaction, its locks and its connection open for as long as the mail server \
              takes to answer.",
        fix: &[(
            Lang::Python,
            "Send it after the commit with transaction.on_commit(lambda: send_mail(...)), or \
             queue a task on commit that sends it. If the email must never be lost, write it \
             to an outbox table inside the transaction and have a worker send it.",
        )],
    },
    Rule {
        code: HTTP_WRITE_IN_TRANSACTION,
        name: "http-write-in-transaction",
        severity: Severity::Error,
        summary: "An HTTP request that changes something elsewhere (POST, PUT, PATCH or DELETE) \
                  is made while the transaction is still open, and a rollback can't undo it.",
        why: "The other service acts on the request straight away. If the transaction then \
              rolls back, the database and the other service disagree: a customer charged for \
              an order that isn't saved, or a record created in the CRM for a user who doesn't \
              exist. The request also holds the transaction open, with its row locks and its \
              pool connection, for as long as the other service takes to answer.",
        fix: &[(
            Lang::Python,
            "Make the call after the commit with transaction.on_commit, or before the \
             transaction and save its result inside it. If it must happen exactly once with \
             the write, record it in an outbox table inside the transaction and have a worker \
             make the call, with an idempotency key if the API takes one.",
        )],
    },
    Rule {
        code: HTTP_READ_IN_TRANSACTION,
        name: "http-read-in-transaction",
        severity: Severity::Warning,
        summary: "An HTTP request is made while the transaction is still open, so the \
                  transaction waits for the other service.",
        why: "A transaction holds its row locks and a database connection until it ends. If \
              the other service takes 30 seconds to answer, so does the transaction: requests \
              that need the same rows queue behind the locks, and the connection pool runs \
              dry. It's a warning, not an error, because a GET changes nothing elsewhere, so a \
              rollback leaves everything consistent; the damage is time.",
        fix: &[(
            Lang::Python,
            "Fetch what you need before the transaction opens, and keep only database work \
             inside it.",
        )],
    },
];

impl Rule {
    /// The fix for a finding in this file.
    pub fn fix_for(&self, path: &str) -> &'static str {
        let lang = Lang::of(path);
        self.fix
            .iter()
            .find(|(l, _)| *l == lang)
            .or_else(|| self.fix.first())
            .map_or("", |(_, text)| text)
    }

    /// Every fix, each labelled with its language when there's more than one.
    pub fn all_fixes(&self) -> String {
        if let [(_, only)] = self.fix {
            return only.to_string();
        }
        self.fix
            .iter()
            .map(|(lang, text)| format!("{}: {text}", lang.name()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn find(code: &str) -> Option<&'static Rule> {
    RULES.iter().find(|r| r.code.eq_ignore_ascii_case(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rule_has_a_fix_and_a_unique_code() {
        assert!(RULES.iter().all(|r| !r.fix.is_empty()));
        let mut codes: Vec<_> = RULES.iter().map(|r| r.code).collect();
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), RULES.len());
    }

    #[test]
    fn codes_are_found_in_any_case() {
        assert_eq!(find("tg001").unwrap().name, "task-before-commit");
        assert!(find("TG999").is_none());
    }
}
