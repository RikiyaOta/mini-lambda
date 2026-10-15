use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// `functions/{name}/manifest.toml` に対応する構造体.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct FunctionManifest {
    /// 実行するファイル.
    pub handler: String,

    /// ランタイム.
    pub runtime: Runtime,

    /// タイムアウト(ms)
    pub timeout_ms: u32,

    /// メモリの上限
    pub memory_mib: u32,

    /// 関数に渡される環境変数
    pub env: HashMap<String, String>,
}

/// 関数定義.
#[derive(Debug, PartialEq, Clone)]
pub struct Function {
    /// 関数名
    pub name: String,

    /// `handler` がおいてあるディレクトリのパス.
    pub base_dir: PathBuf,

    /// マニフェスト.
    pub manifest: FunctionManifest,
}

/// 今回のプロジェクトで定義するランタイム.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    Script,
    Node,
    Python3,
}

impl Function {
    pub fn build(name: &str, base: &Path) -> Result<Function> {
        let path = base.join(format!("functions/{name}/manifest.toml"));
        let file = std::fs::read_to_string(path)?;
        let manifest: FunctionManifest = toml::from_str(&file)?;

        Ok(Function {
            name: name.to_string(),
            base_dir: base.join(format!("functions/{name}/")),
            manifest,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use tempfile::tempdir;

    #[test]
    fn can_read_valid_function_definition() -> Result<()> {
        let name = "test_001";
        let dir = tempdir()?;
        let path = dir.path().join(format!("functions/{name}/manifest.toml"));
        let content = r#"
handler = "index"
runtime = "script"
timeout_ms = 99999
memory_mib = 4086
env = { ENV1 = "hoge", ENV2 = "fuga"}
"#;
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, content)?;

        let f = Function::build(name, dir.path())?;

        assert_eq!(
            f,
            Function {
                name: "test_001".to_string(),
                base_dir: dir.path().join("functions/test_001/"),
                manifest: FunctionManifest {
                    handler: "index".to_string(),
                    runtime: Runtime::Script,
                    timeout_ms: 99999,
                    memory_mib: 4086,
                    env: vec![
                        ("ENV1".to_string(), "hoge".to_string()),
                        ("ENV2".to_string(), "fuga".to_string())
                    ]
                    .into_iter()
                    .collect()
                }
            }
        );

        Ok(())
    }
}
