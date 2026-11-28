use super::{Error, Profile, Result};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Operation {
    View,
    Status,
    History,
    Diff,
    Review,
    Commit,
}

impl Operation {
    pub fn allowed(profile: Profile) -> &'static [Self] {
        match profile {
            Profile::ReadOnly => &[Self::View, Self::Status, Self::History, Self::Diff],
            Profile::LocalCommit => &[
                Self::View,
                Self::Status,
                Self::History,
                Self::Diff,
                Self::Review,
                Self::Commit,
            ],
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::View => "get_current_view",
            Self::Status => "get_status",
            Self::History => "get_history",
            Self::Diff => "get_diff",
            Self::Review => "review_staged",
            Self::Commit => "commit_staged",
        }
    }

    pub fn schema(self) -> Value {
        let id = json!({"type":"string","minLength":1,"maxLength":128});
        let opt_id = json!({"type":["string","null"]});
        let string = json!({"type":"string"});
        let time = json!({"type":"integer"});
        let array = json!({"type":"array","items":{"type":"object"}});
        let (description, args, required, output) = match self {
            Self::Review => (
                "Review the entire Git index, including other staged files. Inspect the returned diff_handle before committing.",
                json!({}),
                vec![],
                json!({"worktree_id":string,"captured_at_ms":time,"branch":string,
                    "expected_head":opt_id,"files":array,"review_id":string,"diff_handle":string}),
            ),
            Self::Commit => (
                "Create one ordinary local commit from the entire reviewed index. Reuse request_id and the identical payload on retry.",
                json!({"review_id":id,"request_id":id,"message":{"type":"string","minLength":1,"maxLength":16384}}),
                vec!["review_id", "request_id", "message"],
                json!({"worktree_id":string,"branch":string,"parent":opt_id,"commit":string,"tree":string}),
            ),
            Self::View => (
                "Capture the authorized foreground tab. Pass its diff_handle to get_diff for the displayed patch.",
                json!({}),
                vec![],
                json!({"worktree_id":string,"active_pane":string,"captured_at_ms":time,"diff_handle":opt_id,"view_handle":opt_id}),
            ),
            Self::Status => (
                "Inspect local staged, unstaged, untracked and conflicted files. Remote information reflects local refs.",
                json!({}),
                vec![],
                json!({"worktree_id":string,"captured_at_ms":time,"staged":array,"unstaged":array,"untracked":array,"conflicted":array}),
            ),
            Self::History => (
                "Read a bounded immutable page of local commits. An optional view_handle applies captured graph filters. Continue with cursor alone.",
                json!({"view_handle":id,"cursor":id,"limit":{"type":"integer","minimum":1,"maximum":100}}),
                vec![],
                json!({"worktree_id":string,"commits":array,"captured_at_ms":time,"next_cursor":opt_id,"truncated":{"type":"boolean"}}),
            ),
            Self::Diff => (
                "Read immutable captured patch text. Use the original diff_handle on every page. Loading requires a fresh view.",
                json!({"diff_handle":id,"cursor":id}),
                vec!["diff_handle"],
                json!({"worktree_id":string,"source":string,"state":string,"text":string,"captured_at_ms":time,"next_cursor":opt_id}),
            ),
        };
        let required_output = output.as_object().unwrap().keys().collect::<Vec<_>>();
        json!({"name":self.name(),"description":description,
            "inputSchema":{"type":"object","properties":args,"required":required,"additionalProperties":false},
            "outputSchema":{"type":"object","properties":output,"required":required_output},
            "annotations":{"readOnlyHint":self != Self::Commit,"openWorldHint":false}})
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Empty {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HistoryArgs {
    pub view_handle: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DiffArgs {
    pub diff_handle: String,
    pub cursor: Option<String>,
}

pub(super) fn arguments<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    // Serde also accepts positional arrays for derived structs. MCP tools accept named fields only.
    if !value.is_object() || value.as_object().unwrap().values().any(Value::is_null) {
        return Err(Error::invalid(
            "Supply a JSON object with the tool's declared fields.",
        ));
    }
    serde_json::from_value(value)
        .map_err(|_| Error::invalid("Use only the named fields and types in the tool schema."))
}

pub(super) fn handle(s: &str) -> Result<&str> {
    if s.is_empty() || s.len() > 128 {
        return Err(Error::invalid("Supply a server-issued handle."));
    }
    Ok(s)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CommitArgs {
    pub review_id: String,
    pub message: String,
    pub request_id: String,
}

impl CommitArgs {
    pub fn validate(&self) -> Result<()> {
        handle(&self.review_id)?;
        if self.request_id.trim().is_empty()
            || self.request_id.len() > 128
            || self.request_id.chars().any(char::is_control)
        {
            return Err(Error::invalid(
                "Supply a request ID of 1 to 128 bytes without control characters.",
            ));
        }
        if self.message.trim().is_empty()
            || self.message.len() > 16384
            || self.message.contains('\0')
        {
            return Err(Error::invalid(
                "Supply a nonblank commit message of at most 16384 bytes without NUL.",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profiles_share_one_closed_allowlist_and_commit_accepts_only_a_review() {
        let names = |p| {
            Operation::allowed(p)
                .iter()
                .map(|op| op.name())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(Profile::ReadOnly),
            ["get_current_view", "get_status", "get_history", "get_diff"]
        );
        assert_eq!(
            &names(Profile::LocalCommit)[4..],
            &["review_staged", "commit_staged"]
        );
        for op in Operation::allowed(Profile::LocalCommit) {
            let schema = op.schema();
            assert_eq!(schema["inputSchema"]["additionalProperties"], false);
            assert_eq!(
                schema["annotations"]["readOnlyHint"],
                *op != Operation::Commit
            );
        }
        for extra in ["amend", "push", "root", "token", "args", "permission"] {
            let mut args = json!({"request_id":"one","review_id":"id","message":"Local"});
            args[extra] = json!(true);
            assert!(arguments::<CommitArgs>(args).is_err());
        }
    }
}
