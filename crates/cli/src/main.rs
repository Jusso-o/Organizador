use organizador_core::{carregar_config, planejar};
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 3 {
        eprintln!("Uso: cli <pasta> <regras.toml>");
        return;
    }

    let pasta = Path::new(&args[1]);

    let config = match carregar_config(Path::new(&args[2])) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Erro ao ler as regras: {e}");
            return;
        }
    };

    match planejar(pasta, &config) {
        Ok(plano) => {
            println!("SIMULAÇÃO: {} arquivo(s) seriam movidos\n", plano.len());
            for acao in &plano {
                println!(
                    "[{}] {} -> {}",
                    acao.regra,
                    acao.origem.display(),
                    acao.destino.display()
                );
            }
        }
        Err(e) => eprintln!("Erro ao ler a pasta: {e}"),
    }
}