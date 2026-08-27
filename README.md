# Desjanjador

Desbloqueia o **Go Live e a câmera do Discord no Brasil**. Roteia **só o gateway** do
Discord por uma saída fora do Brasil (proxy gratuito testado ou Tor); todo o resto sai
direto, em velocidade normal. App em **Tauri** (Rust + React), Windows, portable (sem
assinatura).

## Como usar

1. Rode `install.ps1` (ou abra o `desjanjador.exe` direto).
2. Ligue **Ativo** e espere a saída ficar pronta.
3. **Reinicie o Discord** (Stable / PTB / Canary) — o gateway precisa nascer pela saída.
4. Entre numa call de voz e teste o **Go Live / câmera**.

> Dica: com o **Tor Browser** aberto, a saída fica bem mais estável que proxy gratuito
> (proxy grátis costuma cair a cada ~40s e derrubar o gateway).

## Botões

- **Instalar BetterDiscord** — injeta o BetterDiscord em qualquer Discord instalado.
- **Aplicar patch** — coloca `Desjanjador.plugin.js` no BetterDiscord (rede de
  segurança; o desbloqueio de verdade é o proxy do gateway).

## Rodar (Windows 10 e 11)

Baixe `Desjanjador.bat` **e** `Desjanjador.ps1` (ficam juntos) e dê **dois cliques no
`.bat`**. Não dê dois cliques no `.ps1` — o Windows abre no bloco de notas. (Alternativa:
clique-direito no `.ps1` → **Executar com o PowerShell**.)

Ele pede admin (UAC), garante o WebView2 (Windows 10), baixa o exe mais recente do GitHub
(ou usa um `desjanjador.exe` ao lado), tira a marca da web (bypass do SmartScreen), exclui
a pasta no Defender e abre o app **sem admin**. A inicialização com o Windows liga/desliga
**dentro do app**. Para remover: `Desjanjador.bat -Uninstall`.

## Aviso

Não é assinado — na 1ª execução o SmartScreen pode avisar ("Mais informações →
Executar assim mesmo"). Windows portable **não precisa** de assinatura. Usar isto pode
violar os termos do Discord; use por conta própria.
