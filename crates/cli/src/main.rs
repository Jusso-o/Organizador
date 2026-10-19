use organizador_core::{
    carregar_config, carregar_diario, desfazer_ultimo, executar, monitorar, planejar,
    salvar_diario, Config,
};
use std::path::Path;
use std::sync::atomic::AtomicBool;

const DIARIO: &str = "diario.json";

fn main() {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        Some("simular") if args.len() == 4 => organizar(&args[2], &args[3], false),
        Some("executar") if args.len() == 4 => organizar(&args[2], &args[3], true),
        Some("vigiar") if args.len() == 4 => vigiar(&args[2], &args[3]),
        Some("desfazer") => desfazer(),
        _ => uso(),
    }
}

fn uso() {
    eprintln!("Uso:");
    eprintln!("  cli simular  <pasta> <regras.toml>");
    eprintln!("  cli executar <pasta> <regras.toml>");
    eprintln!("  cli vigiar   <pasta> <regras.toml>");
    eprintln!("  cli desfazer");
}

fn ler_config(caminho: &str) -> Option<Config> {
    match carregar_config(Path::new(caminho)) {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("Erro ao ler as regras: {e}");
            None
        }
    }
}

fn organizar(pasta: &str, regras: &str, de_verdade: bool) {
    let Some(config) = ler_config(regras) else {
        return;
    };

    let plano = match planejar(Path::new(pasta), &config) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Erro ao ler a pasta: {e}");
            return;
        }
    };

    if plano.is_empty() {
        println!("Nada para organizar.");
        return;
    }

    if !de_verdade {
        println!("SIMULAÇÃO: {} arquivo(s) seriam movidos\n", plano.len());
        for acao in &plano {
            println!(
                "[{}] {} -> {}",
                acao.regra,
                acao.origem.display(),
                acao.destino.display()
            );
        }
        println!("\nPara executar de verdade, use o comando 'executar'.");
        return;
    }

    let mut diario = match carregar_diario(Path::new(DIARIO)) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Erro ao ler o diário, nada foi movido: {e}");
            return;
        }
    };

    let (lote, erros) = executar(&plano);
    println!("{} arquivo(s) movido(s).", lote.movimentos.len());
    for e in &erros {
        eprintln!("Aviso: {e}");
    }

    if lote.movimentos.is_empty() {
        return;
    }

    diario.push(lote);
    if let Err(e) = salvar_diario(Path::new(DIARIO), &diario) {
        eprintln!("ERRO ao gravar o diário (os arquivos já foram movidos): {e}");
    }
}

fn vigiar(pasta: &str, regras: &str) {
    let Some(config) = ler_config(regras) else {
        return;
    };

    println!("Vigiando '{pasta}'... (Ctrl+C para parar)");

    // Na CLI ninguém aciona a parada (usamos Ctrl+C), mas a função exige a bandeira.
    let parar = AtomicBool::new(false);

    let resultado = monitorar(Path::new(pasta), &config, Path::new(DIARIO), &parar, |msg| {
        println!("{msg}")
    });

    if let Err(e) = resultado {
        eprintln!("Erro no monitoramento: {e}");
    }
}

fn desfazer() {
    match desfazer_ultimo(Path::new(DIARIO)) {
        Ok((0, avisos)) if avisos.is_empty() => println!("Nada para desfazer."),
        Ok((n, avisos)) => {
            println!("{n} arquivo(s) restaurado(s).");
            for a in avisos {
                eprintln!("Aviso: {a}");
            }
        }
        Err(e) => eprintln!("Erro ao desfazer: {e}"),
    }
}