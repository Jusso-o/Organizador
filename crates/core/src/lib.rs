use serde::Deserialize;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Uma regra: arquivos com estas extensões vão para esta pasta.
#[derive(Debug, Deserialize)]
pub struct Regra {
    pub nome: String,
    pub extensoes: Vec<String>,
    pub destino: String,
}

/// O conteúdo do arquivo de regras.
#[derive(Debug, Deserialize)]
pub struct Config {
    pub regras: Vec<Regra>,
}

/// Uma ação que o programa pretende fazer (ainda não executada).
#[derive(Debug)]
pub struct AcaoPlanejada {
    pub origem: PathBuf,
    pub destino: PathBuf,
    pub regra: String,
}

/// Lê o arquivo de regras (TOML).
pub fn carregar_config(caminho: &Path) -> Result<Config, Box<dyn std::error::Error>> {
    let texto = fs::read_to_string(caminho)?;
    let config: Config = toml::from_str(&texto)?;
    Ok(config)
}

/// Olha a pasta e monta a lista do que seria movido, sem mover nada.
pub fn planejar(pasta: &Path, config: &Config) -> io::Result<Vec<AcaoPlanejada>> {
    let mut plano = Vec::new();

    for entrada in fs::read_dir(pasta)? {
        let caminho = entrada?.path();

        // Ignora subpastas: só queremos arquivos.
        if !caminho.is_file() {
            continue;
        }

        // Pega a extensão em minúsculas; sem extensão, pula o arquivo.
        let Some(ext) = caminho.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        let ext = ext.to_lowercase();

        // Procura a primeira regra que combina com a extensão.
        for regra in &config.regras {
            if regra.extensoes.iter().any(|e| e.to_lowercase() == ext) {
                let nome_arquivo = caminho.file_name().unwrap();
                let destino = pasta.join(&regra.destino).join(nome_arquivo);

                plano.push(AcaoPlanejada {
                    origem: caminho.clone(),
                    destino,
                    regra: regra.nome.clone(),
                });
                break; // primeira regra que casar vence
            }
        }
    }

    Ok(plano)
}