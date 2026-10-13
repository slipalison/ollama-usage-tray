# ollama-usage-tray

Um indicador de bandeja (system tray) leve para Linux/GNOME que monitora o consumo do seu plano **Ollama Cloud** na barra superior — limites de sessão e semana ou créditos do mês, requests e quando o plano renova. Tudo em Rust puro, sem GTK, ~11 MB de RAM.

![ícone](assets/icon.svg)

## Recursos

- **Título na bandeja**: `S43% · W10%` (plano legado: quanto da sessão e da semana já foi usado) ou `$28/$100` (créditos mensais: dólares usados vs. créditos do plano)
- **Ícone customizado** com anel de status (azul → laranja ≥70% → vermelho ≥90%), pelo limite mais apertado
- **Menu completo**: plano, sessão e semana ou créditos do mês, horário de renovação (vindo da API, no fuso local), créditos comprados, requests dos últimos 7 dias
- **Detecção automática do modelo de preço**: contas antigas (limites de sessão/semana) e o novo modelo de créditos mensais em dólares são ambos suportados sem configuração
- **Gestão de API key**: `set-key`, `show-key`, `clear-key`, `key-info`; cópia da key (mascarada) direto do menu
- **Single-instance** (lock via `fd-lock`) e **autostart** no login
- Atualização a cada 5 minutos + item "Atualizar agora"

## Requisitos

- Linux com bandeja **StatusNotifierItem** (GNOME + extensão AppIndicator, KDE, XFCE, etc.)
- `libssl-dev`, `pkg-config` (build)
- Runtime: `python3` com `python3-gi` + `gir1.2-rsvg` (renderização do ícone), `xclip` (copiar key), `zenity` (diálogos), `nautilus` ou similar (abrir pasta)
- Conta Ollama com API key (a do opencode funciona: `~/.local/share/opencode/auth.json` → `ollama-cloud.key`)

## Instalação

```bash
git clone https://github.com/slipalison-ai/ollama-usage-tray.git
cd ollama-usage-tray
./install.sh
```

O script compila com `cargo build --release`, instala o binário em `~/.local/bin`, os assets em `~/.local/share/ollama-usage-tray/`, o ícone no tema hicolor, o autostart em `~/.config/autostart/` e lança o app.

### Desinstalação

```bash
./uninstall.sh          # no clone do repositório
# ou, para remover também a key local salva via set-key:
./uninstall.sh --purge
```

## Uso

O app fica na bandeja. Clique com o botão direito para o menu; passe o mouse para o tooltip com todos os detalhes.

### CLI

```bash
ollama-usage-tray                 # inicia o tray (segunda instância sai sozinha)
ollama-usage-tray key-info        # valida a key e imprime plano, uso e renovação no terminal
ollama-usage-tray set-key <KEY>   # salva a API key (ou via stdin: echo <KEY> | ollama-usage-tray set-key)
ollama-usage-tray show-key        # mostra a key mascarada + origem
ollama-usage-tray clear-key       # remove a key salva localmente
```

### Origem da API key (precedência)

1. `OLLAMA_USAGE_TRAY_KEY` (variável de ambiente)
2. `~/.config/ollama-usage-tray.key` (arquivo 0600, via `set-key`)
3. `~/.local/share/opencode/auth.json` (`ollama-cloud.key`) — integração com opencode

## Endpoints usados

| Endpoint | Uso |
|---|---|
| `GET https://ollama.com/api/balance` | Plano legado: `included.session` e `included.weekly` (`remaining_percent`, `resets_at`). Créditos mensais: `included.balance_usd`, `allowance_usd` e `period`. Os dois: `purchased.balance_usd` |
| `GET https://ollama.com/api/usage?range=7d` | `totals.request_count` e, nos planos novos, `totals.usage_usd` |
| `POST https://ollama.com/api/me` | Nome do plano |

Documentação: [Balance](https://docs.ollama.com/api/balance) e [Cloud usage](https://docs.ollama.com/api/cloud-usage).

O `/api/balance` é o dado essencial: se `/api/me` ou `/api/usage` falharem, o tray continua mostrando os limites, com um aviso no tooltip. Os endpoints aceitam 10 requisições por minuto por usuário, somando todas as keys e dispositivos; o tray consulta a cada 5 minutos e, num `429`, espera o `Retry-After` antes de tentar de novo (inclusive no "Atualizar agora").

## Segurança

- A key nunca aparece completa na UI (máscara `c10c••••Wpm9`); "Copiar API key" envia a key real para o clipboard via `xclip`
- O arquivo de key local é criado com modo `0600`
- Sem telemetria, sem dados para terceiros — o app só fala com `ollama.com`

## Licença

MIT