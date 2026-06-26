// ── Módulo de Versionamento Git (auto-commit) ───────────────────────────────
//
// Versiona o conteúdo da Wiki num repositório git dedicado, enraizado em
// `.advwiki/`. Habilitado pela flag `--autocommit`. Faz commit automático
// (debounced) de cada rajada de mudanças e push best-effort para o upstream
// do branch atual.
//
// Decisões de design:
//   - Shell out para o binário `git` (não uma lib): herda config, credential
//     helper e SSH agent do ambiente — essencial para o push autenticar.
//   - Repo enraizado em `.advwiki/`: isolamento total de qualquer repo de
//     projeto que contenha a wiki. Um `.gitignore` pré-configurado versiona só
//     o conteúdo útil (pages/, sources/, metadata/) e ignora artefatos
//     rebuildáveis (index/ do Tantivy, proposals/, backups de migração).
//   - Só a instância primary commita (o setup é feito quando ela assume o
//     papel), evitando corrida entre múltiplas instâncias.
//   - Commit hookado no stream de `WikiEvent` (não em eventos brutos de FS):
//     escritas do git em `.git/` e do Tantivy em `index/` não disparam commit.

use crate::watcher::WikiEvent;
use anyhow::{Context, bail};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::mpsc;

/// Janela de quietude para agrupar uma rajada de mudanças num único commit.
const DEBOUNCE: Duration = Duration::from_secs(3);

/// Conteúdo do `.gitignore` gravado em `.advwiki/` na inicialização.
/// Versiona o conteúdo útil; ignora artefatos binários/transitórios.
const GITIGNORE_CONTENTS: &str = "\
# Gerado pelo AdvWiki (--autocommit). Versiona apenas conteúdo útil.
index/
embeddings/
proposals/
.backup-pre-wikilinks-*/
*.tmp
";

/// Versionador git da Wiki. Opera no diretório `.advwiki/`.
pub struct GitSync {
    /// Diretório `.advwiki/` — raiz do repositório git da wiki.
    wiki_dir: PathBuf,
    /// Se `true`, tenta `git push` best-effort após cada commit.
    push: bool,
}

impl GitSync {
    /// Cria um versionador apontando para `wiki_dir` (o `.advwiki/`).
    pub fn new(wiki_dir: &Path, push: bool) -> Self {
        Self {
            wiki_dir: wiki_dir.to_path_buf(),
            push,
        }
    }

    /// Executa um comando `git` com `current_dir` = `.advwiki/`.
    async fn run_git(&self, args: &[&str]) -> anyhow::Result<std::process::Output> {
        Command::new("git")
            .args(args)
            .current_dir(&self.wiki_dir)
            .output()
            .await
            .with_context(|| format!("Falha ao executar `git {}`", args.join(" ")))
    }

    /// Garante que `.advwiki/` é um repositório git pronto para uso. Idempotente:
    ///   - `git init` se ainda não houver `.git`;
    ///   - grava o `.gitignore` pré-configurado (se ausente);
    ///   - define uma identidade default se a config ambiente não resolver uma.
    pub async fn ensure_repo(&self) -> anyhow::Result<()> {
        if !self.wiki_dir.join(".git").exists() {
            let out = self.run_git(&["init"]).await?;
            if !out.status.success() {
                bail!(
                    "git init falhou em {}: {}",
                    self.wiki_dir.display(),
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            tracing::info!(dir = %self.wiki_dir.display(), "Repositório git da wiki inicializado");
        }

        self.write_gitignore().await?;
        self.ensure_identity().await?;
        Ok(())
    }

    /// Grava o `.gitignore` em `.advwiki/` se ainda não existir (não sobrescreve
    /// um arquivo customizado pelo usuário).
    async fn write_gitignore(&self) -> anyhow::Result<()> {
        let path = self.wiki_dir.join(".gitignore");
        if path.exists() {
            return Ok(());
        }
        tokio::fs::write(&path, GITIGNORE_CONTENTS)
            .await
            .with_context(|| format!("Falha ao gravar .gitignore: {}", path.display()))?;
        tracing::debug!(path = %path.display(), ".gitignore criado");
        Ok(())
    }

    /// Define `user.name`/`user.email` locais se o git não resolver uma
    /// identidade (global/ambiente), evitando que o commit falhe.
    async fn ensure_identity(&self) -> anyhow::Result<()> {
        let email = self.run_git(&["config", "user.email"]).await?;
        let has_identity = email.status.success() && !email.stdout.is_empty();
        if has_identity {
            return Ok(());
        }
        self.run_git(&["config", "user.email", "advwiki@localhost"])
            .await?;
        self.run_git(&["config", "user.name", "AdvWiki"]).await?;
        tracing::debug!("Identidade git default configurada para o repo da wiki");
        Ok(())
    }

    /// Faz `git add -A` + `git commit`. Retorna `Ok(false)` se não havia nada
    /// para commitar. Em caso de commit, dispara o push best-effort.
    pub async fn commit(&self, message: &str) -> anyhow::Result<bool> {
        self.run_git(&["add", "-A"]).await?;

        // `git diff --cached --quiet` sai com 0 quando NÃO há nada staged.
        let staged = self.run_git(&["diff", "--cached", "--quiet"]).await?;
        if staged.status.success() {
            return Ok(false);
        }

        // Commits da wiki são gerados pela máquina; desabilitamos a assinatura
        // (`-c commit.gpgsign=false`) para não travar nem falhar em ambientes
        // com signing obrigatório/interativo configurado globalmente.
        let out = self
            .run_git(&["-c", "commit.gpgsign=false", "commit", "-m", message])
            .await?;
        if !out.status.success() {
            bail!(
                "git commit falhou: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        tracing::info!(message, "Commit da wiki criado");

        if self.push {
            self.try_push().await;
        }
        Ok(true)
    }

    /// Push best-effort: só empurra se houver upstream configurado para o branch
    /// atual; falhas (rede, auth) são apenas logadas, nunca propagadas.
    async fn try_push(&self) {
        let upstream = self
            .run_git(&["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
            .await;
        let has_upstream = matches!(&upstream, Ok(o) if o.status.success());
        if !has_upstream {
            tracing::debug!("Sem upstream configurado; push da wiki ignorado");
            return;
        }

        match self.run_git(&["push"]).await {
            Ok(o) if o.status.success() => tracing::info!("Push da wiki concluído"),
            Ok(o) => tracing::warn!(
                stderr = %String::from_utf8_lossy(&o.stderr),
                "git push da wiki falhou (seguindo — best-effort)"
            ),
            Err(e) => tracing::warn!(error = %e, "Erro ao executar git push da wiki"),
        }
    }
}

/// Sobe a task de commit com debounce e retorna o canal para enfileirar
/// descrições de mudança. Cada item descreve uma operação (ex.: "atualiza
/// queue-service"); a task agrupa uma rajada e gera um único commit.
pub fn spawn_committer(git: Arc<GitSync>) -> mpsc::UnboundedSender<String> {
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    tokio::spawn(async move {
        // Bloqueia até a primeira mudança; depois coleta tudo que chegar dentro
        // da janela de DEBOUNCE sem novidades.
        while let Some(first) = rx.recv().await {
            let mut descriptions = vec![first];
            loop {
                match tokio::time::timeout(DEBOUNCE, rx.recv()).await {
                    Ok(Some(desc)) => descriptions.push(desc),
                    Ok(None) => break, // canal fechado durante a coleta
                    Err(_) => break,   // timeout → janela de quietude atingida
                }
            }

            let message = build_commit_message(&descriptions);
            match git.commit(&message).await {
                Ok(true) => {}
                Ok(false) => tracing::debug!("Nada para commitar na wiki"),
                Err(e) => tracing::error!(error = %e, "Falha ao commitar a wiki"),
            }
        }
        tracing::debug!("Committer da wiki encerrado (canal fechado)");
    });

    tx
}

/// Descreve a mudança de um `WikiEvent` para a mensagem de commit, ou `None`
/// para eventos que não representam conteúdo versionado (log, rawindex, ruído).
pub fn commit_description(event: &WikiEvent) -> Option<String> {
    match event {
        WikiEvent::PageCreated { slug } => Some(format!("cria {slug}")),
        WikiEvent::PageUpdated { slug } => Some(format!("atualiza {slug}")),
        WikiEvent::PageDeleted { slug } => Some(format!("remove {slug}")),
        WikiEvent::RawSourceUpdated { source_id } => Some(format!("ingere fonte {source_id}")),
        WikiEvent::RawSourceDeleted { source_id } => Some(format!("remove fonte {source_id}")),
        // `rawindex.md` e o log ficam fora do repo `.advwiki/`; Unknown é ruído.
        WikiEvent::IndexChanged | WikiEvent::LogChanged | WikiEvent::Unknown(_) => None,
    }
}

/// Constrói a mensagem de commit a partir das descrições coletadas, deduplicando
/// e mantendo a ordem de chegada. Cai num resumo por contagem se ficar longa.
fn build_commit_message(descriptions: &[String]) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut parts: Vec<&str> = Vec::new();
    for d in descriptions {
        if seen.insert(d.as_str()) {
            parts.push(d.as_str());
        }
    }

    let summary = parts.join(", ");
    if summary.chars().count() <= 60 {
        format!("wiki: {summary}")
    } else {
        format!("wiki: {} alterações", parts.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Pula o teste silenciosamente se o binário `git` não estiver disponível.
    async fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    async fn run_git_in(dir: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .await
            .unwrap()
    }

    #[test]
    fn test_commit_description_maps_events() {
        assert_eq!(
            commit_description(&WikiEvent::PageCreated { slug: "home".into() }).as_deref(),
            Some("cria home")
        );
        assert_eq!(
            commit_description(&WikiEvent::PageDeleted { slug: "old".into() }).as_deref(),
            Some("remove old")
        );
        assert_eq!(
            commit_description(&WikiEvent::RawSourceUpdated { source_id: "abc".into() }).as_deref(),
            Some("ingere fonte abc")
        );
        // Eventos não versionados não geram descrição.
        assert!(commit_description(&WikiEvent::IndexChanged).is_none());
        assert!(commit_description(&WikiEvent::LogChanged).is_none());
        assert!(commit_description(&WikiEvent::Unknown("x".into())).is_none());
    }

    #[test]
    fn test_build_commit_message_dedups_and_orders() {
        let descs = vec![
            "atualiza a".to_string(),
            "cria b".to_string(),
            "atualiza a".to_string(),
        ];
        assert_eq!(build_commit_message(&descs), "wiki: atualiza a, cria b");
    }

    #[test]
    fn test_build_commit_message_falls_back_when_long() {
        let descs: Vec<String> = (0..20).map(|i| format!("atualiza pagina-numero-{i}")).collect();
        let msg = build_commit_message(&descs);
        assert!(msg.starts_with("wiki: 20 alterações"), "msg = {msg}");
    }

    #[tokio::test]
    async fn test_ensure_repo_inits_and_writes_gitignore() {
        if !git_available().await {
            eprintln!("git indisponível — pulando");
            return;
        }
        let dir = TempDir::new().unwrap();
        let wiki_dir = dir.path().join(".advwiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();

        let git = GitSync::new(&wiki_dir, false);
        git.ensure_repo().await.unwrap();

        assert!(wiki_dir.join(".git").exists(), "deveria existir .git");
        let gitignore = std::fs::read_to_string(wiki_dir.join(".gitignore")).unwrap();
        assert!(gitignore.contains("index/"));
        assert!(gitignore.contains("embeddings/"));
        assert!(gitignore.contains("proposals/"));
        assert!(gitignore.contains(".backup-pre-wikilinks-*/"));

        // Idempotente: rodar de novo não falha.
        git.ensure_repo().await.unwrap();
    }

    #[tokio::test]
    async fn test_commit_creates_commit_and_is_noop_when_clean() {
        if !git_available().await {
            eprintln!("git indisponível — pulando");
            return;
        }
        let dir = TempDir::new().unwrap();
        let wiki_dir = dir.path().join(".advwiki");
        std::fs::create_dir_all(wiki_dir.join("pages")).unwrap();

        let git = GitSync::new(&wiki_dir, false /* sem push em teste */);
        git.ensure_repo().await.unwrap();

        // Primeiro commit: há conteúdo novo (.gitignore + página).
        std::fs::write(wiki_dir.join("pages").join("home.md"), "# Home").unwrap();
        assert!(git.commit("wiki: cria home").await.unwrap(), "deveria commitar");

        // Sem mudanças: no-op.
        assert!(!git.commit("wiki: nada").await.unwrap(), "deveria ser no-op");

        // Confere que há exatamente um commit no log.
        let log = run_git_in(&wiki_dir, &["log", "--oneline"]).await;
        let count = String::from_utf8_lossy(&log.stdout).lines().count();
        assert_eq!(count, 1, "esperava 1 commit");

        // O índice Tantivy (index/) é ignorado pelo .gitignore.
        std::fs::create_dir_all(wiki_dir.join("index")).unwrap();
        std::fs::write(wiki_dir.join("index").join("meta.json"), "{}").unwrap();
        assert!(!git.commit("wiki: ignora index").await.unwrap(), "index/ não deve ser versionado");
    }
}
