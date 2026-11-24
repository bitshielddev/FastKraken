use crate::{github::CreatedRepo, repository::Repository};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{ErrorKind, Write},
    path::PathBuf,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Receipt {
    pub client_id: String,
    pub account: u64,
    pub owner: String,
    pub name: String,
    pub private: bool,
    pub remote: String,
    pub branch: Option<(String, String)>,
    pub created: Option<CreatedRepo>,
    pub tracking_only: bool,
}

impl Receipt {
    fn path(repo: &Repository) -> PathBuf {
        repo.git_dir().join("fastkraken-publish.json")
    }

    pub fn load(repo: &Repository) -> Result<Option<Self>, String> {
        let path = Self::path(repo);
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| "Cannot read fastkraken-publish.json in the Git directory.".into()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("Read saved GitHub publication: {error}")),
        }
    }

    pub fn save(&self, repo: &Repository, new: bool) -> Result<(), String> {
        let mut file = tempfile::NamedTempFile::new_in(repo.git_dir())
            .map_err(|e| format!("Prepare GitHub publish record: {e}"))?;
        serde_json::to_writer(&mut file, self).map_err(|e| e.to_string())?;
        file.flush()
            .and_then(|_| file.as_file().sync_all())
            .map_err(|e| e.to_string())?;
        let result = if new {
            file.persist_noclobber(Self::path(repo))
        } else {
            file.persist(Self::path(repo))
        };
        result
            .map(|_| ())
            .map_err(|e| format!("Save GitHub publish record: {e}"))
    }

    pub fn clear(repo: &Repository) -> Result<(), String> {
        match fs::remove_file(Self::path(repo)) {
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            result => result.map_err(|e| format!("Clear saved GitHub publication: {e}")),
        }
    }

    pub fn verify_target(&self, remote: &CreatedRepo) -> Result<(), String> {
        if !remote.full_name.eq_ignore_ascii_case(&self.full_name())
            || remote.private != self.private
        {
            return Err("GitHub returned a different owner, name or visibility.".into());
        }
        if let Some(saved) = &self.created
            && saved.id != remote.id
        {
            return Err(
                "This name now belongs to a different GitHub repository. Review the destination."
                    .into(),
            );
        }
        Ok(())
    }

    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;

    #[test]
    fn pending_request_survives_reopen_and_another_create_cannot_replace_it() {
        let f = Fixture::new();
        let mut request = Receipt {
            client_id: "public-client".into(),
            account: 7,
            owner: "cedar".into(),
            name: "field-notes".into(),
            private: true,
            remote: "origin".into(),
            branch: Some(("topic/notes".into(), "abcd".repeat(10))),
            created: None,
            tracking_only: false,
        };
        request.save(&f.repo, true).unwrap();
        let other = Receipt {
            name: "different".into(),
            ..request.clone()
        };
        assert!(other.save(&f.repo, true).is_err());
        assert_eq!(Receipt::load(&f.repo).unwrap(), Some(request.clone()));

        let remote = CreatedRepo {
            id: 12,
            full_name: "Cedar/field-notes".into(),
            clone_url: "https://github.com/Cedar/field-notes.git".into(),
            private: true,
        };
        assert!(request.verify_target(&remote).is_ok());
        request.created = Some(remote.clone());
        request.tracking_only = true;
        request.save(&f.repo, false).unwrap();
        assert_eq!(Receipt::load(&f.repo).unwrap(), Some(request.clone()));

        let replacement = CreatedRepo {
            id: 13,
            ..remote.clone()
        };
        let changed_visibility = CreatedRepo {
            private: false,
            ..remote.clone()
        };
        let different_owner = CreatedRepo {
            full_name: "oak/field-notes".into(),
            ..remote
        };
        for different in [replacement, changed_visibility, different_owner] {
            assert!(request.verify_target(&different).is_err());
        }

        Receipt::clear(&f.repo).unwrap();
        assert!(Receipt::load(&f.repo).unwrap().is_none());
    }
}
