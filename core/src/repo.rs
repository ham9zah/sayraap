//! The chain of steps, kept in Git through libgit2. The user never installs
//! or runs Git: an accepted task is one commit, a rejected one is rolled back.

use crate::Message;
use git2::build::CheckoutBuilder;
use git2::{ErrorCode, Repository, ResetType, Signature, StatusOptions};
use std::path::Path;

pub struct Repo {
    inner: Repository,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoError {
    /// The project folder is not the root of a Git repository.
    NotFound { path: String },
    /// The repository has no commit yet, so there is nothing to roll back to.
    NoCommit,
    /// There are unsaved changes; a rollback would destroy them.
    Dirty { files: Vec<String> },
    /// Anything else libgit2 reports.
    Failed { detail: String },
}

impl RepoError {
    pub fn message(&self) -> Message {
        match self {
            RepoError::NotFound { path } => Message::new("repo-not-found").arg("path", path),
            RepoError::NoCommit => Message::new("repo-no-commit"),
            RepoError::Dirty { files } => Message::new("repo-dirty")
                .num("count", files.len())
                .arg("files", files.join(", ")),
            RepoError::Failed { detail } => Message::new("repo-failed").arg("detail", detail),
        }
    }
}

impl From<git2::Error> for RepoError {
    fn from(e: git2::Error) -> Self {
        RepoError::Failed { detail: e.message().to_string() }
    }
}

impl Repo {
    /// Opens the project and checks it has a commit to return to.
    pub fn open(project: &Path) -> Result<Self, RepoError> {
        let inner = Repository::open(project)
            .map_err(|_| RepoError::NotFound { path: project.display().to_string() })?;
        match inner.head() {
            Ok(_) => {}
            Err(e) if e.code() == ErrorCode::UnbornBranch => return Err(RepoError::NoCommit),
            Err(e) => return Err(e.into()),
        }
        Ok(Self { inner })
    }

    /// Files that differ from the last commit, including new ones. Ignored files are skipped.
    pub fn changed_files(&self) -> Result<Vec<String>, RepoError> {
        let mut options = StatusOptions::new();
        options.include_untracked(true).recurse_untracked_dirs(true).include_ignored(false);
        let statuses = self.inner.statuses(Some(&mut options))?;
        let mut files: Vec<String> =
            statuses.iter().filter_map(|s| s.path().map(str::to_string)).collect();
        files.sort();
        Ok(files)
    }

    /// Fails unless the project is exactly at its last commit.
    pub fn require_clean(&self) -> Result<(), RepoError> {
        let files = self.changed_files()?;
        if files.is_empty() { Ok(()) } else { Err(RepoError::Dirty { files }) }
    }

    /// Saves one file as a new step and returns the commit id.
    pub fn commit_file(&self, file: &str, summary: &str) -> Result<String, RepoError> {
        let mut index = self.inner.index()?;
        index.add_path(Path::new(file))?;
        index.write()?;
        let tree = self.inner.find_tree(index.write_tree()?)?;
        let parent = self.inner.head()?.peel_to_commit()?;
        let author = match self.inner.signature() {
            Ok(signature) => signature,
            Err(_) => Signature::now("sayraap", "sayraap@localhost")?,
        };
        let id = self.inner.commit(Some("HEAD"), &author, &author, summary, &tree, &[&parent])?;
        Ok(id.to_string())
    }

    /// Returns the project to its last commit: changed files are restored
    /// and new files are removed.
    pub fn rollback(&self) -> Result<(), RepoError> {
        let head = self.inner.head()?.peel_to_commit()?;
        let mut checkout = CheckoutBuilder::new();
        checkout.force();
        self.inner.reset(head.as_object(), ResetType::Hard, Some(&mut checkout))?;

        // The reset restores tracked files only; whatever still differs is new.
        let Some(root) = self.inner.workdir().map(Path::to_path_buf) else {
            return Ok(());
        };
        for file in self.changed_files()? {
            let path = root.join(&file);
            std::fs::remove_file(&path)
                .map_err(|e| RepoError::Failed { detail: e.to_string() })?;
            // Remove folders the new file left empty, never the root itself.
            let mut folder = path.parent();
            while let Some(dir) = folder {
                if dir == root || std::fs::remove_dir(dir).is_err() {
                    break;
                }
                folder = dir.parent();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use tempfile::TempDir;

    /// A throwaway repository with one committed file, `greet.rs`.
    pub fn project(content: &str) -> (TempDir, Repo) {
        let dir = TempDir::new().unwrap();
        let inner = Repository::init(dir.path()).unwrap();
        std::fs::write(dir.path().join("greet.rs"), content).unwrap();
        {
            let mut index = inner.index().unwrap();
            index.add_path(Path::new("greet.rs")).unwrap();
            index.write().unwrap();
            let tree = inner.find_tree(index.write_tree().unwrap()).unwrap();
            let author = Signature::now("test", "test@localhost").unwrap();
            inner.commit(Some("HEAD"), &author, &author, "start", &tree, &[]).unwrap();
        }
        let repo = Repo::open(dir.path()).unwrap();
        (dir, repo)
    }
}

#[cfg(test)]
mod tests {
    use super::testing::project;
    use super::*;

    #[test]
    fn refuses_folders_that_cannot_be_rolled_back() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(matches!(Repo::open(dir.path()), Err(RepoError::NotFound { .. })));
        Repository::init(dir.path()).unwrap();
        assert!(matches!(Repo::open(dir.path()), Err(RepoError::NoCommit)));
    }

    #[test]
    fn commit_makes_the_project_clean_again() {
        let (dir, repo) = project("one\n");
        repo.require_clean().unwrap();
        std::fs::write(dir.path().join("greet.rs"), "two\n").unwrap();
        assert_eq!(repo.changed_files().unwrap(), ["greet.rs"]);
        let id = repo.commit_file("greet.rs", "change").unwrap();
        assert_eq!(id.len(), 40);
        repo.require_clean().unwrap();
    }

    #[test]
    fn rollback_restores_changed_files_and_removes_new_ones() {
        let (dir, repo) = project("one\n");
        std::fs::write(dir.path().join("greet.rs"), "broken\n").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/stray.txt"), "x").unwrap();
        assert!(matches!(repo.require_clean(), Err(RepoError::Dirty { .. })));
        repo.rollback().unwrap();
        repo.require_clean().unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("greet.rs")).unwrap(), "one\n");
        assert!(!dir.path().join("sub").exists());
    }
}
