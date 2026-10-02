# Item F: avaliação das peças do fastframe (v1.0.117)

fastframe (https://github.com/crmne/fastframe, MIT, egui 0.36.1/glow) é a base
que o upstream ZapFast passou a usar. Regra da rodada: dependência por `git` com `rev`
fixo, uma peça por PR, só adotar se resolver um problema real do Vespera com
teste/medida mostrando igual ou melhor, `.exe` até 1 MB maior no total.

Veredito da rodada 10: **nenhuma peça adotada**. Cada candidata foi conferida
contra o problema que motivaria a adoção; em todos os casos o Vespera já tem
a sua versão funcionando ou o ganho não se confirma. Registrar o motivo aqui
é o comportamento previsto ("se uma peça brigar com o que o Vespera já tem,
pule e registre o motivo").

## 1. `fastframe-emoji` (não adotar)

Motivação seria emoji renderizado errado (reclamação antiga do dono). O
pipeline atual (`src/markup.rs` + `src/emoji.rs`: placeholder no layout,
bitmap colorido do sistema pintado depois, sequências via ligaduras GSUB)
entrega emoji colorido correto nas capturas desta rodada (lista, conversa,
seletor e fileira de reações do Status, `status-r10/after-*.png`). Trocar um
pipeline funcionando por um crate `git` instável, sem medida mostrando
melhora, viola a regra de adoção.

## 2. `fastframe-text` + `fastframe-fonts` (não adotar)

Suavização do Windows e encaixe no pixel exigiriam prova de nitidez (prints
em 100% e 150%). Não há reclamação de nitidez nesta rodada e a troca da
pilha de fontes arrisca regressão nos quatro idiomas (inclui chinês
simplificado). Sem problema demonstrado, sem adoção.

## 3. `fastframe-update` (não trocar: verificação assinada adiada)

O atualizador do Vespera já cobre tudo que a regra exige antes da troca:
procura ao abrir, download com `checksums.txt`, "Instalar e reiniciar",
portátil e instalador, sem instalar durante chamada, auxiliar com volta
atrás (`restore_and_restart` em `src/updates/install.rs`). O que falta é a
verificação **assinada** do manifesto (o próprio código marca:
"until release signatures cover the manifest, not just the payload").
Assinar o manifesto exige chaves do fork e rolagem do auxiliar, ou seja,
infra de release, não um PR de código: fica para uma rodada de
empacotamento, com `PUBLISH_AUR` e segredos configurados.

## 4. `fastframe-tray` + `fastframe-shell` (não adotar)

O congelamento no duplo clique da bandeja não se reproduz nesta rodada e a
bandeja atual (ksni no Linux, tray-icon no Windows/macOS) passa no CI das
três plataformas. Trocar o ciclo de vida da janela sem reprodução do bug
seria risco sem medida.

## 5. `fastframe-log` (não adotar)

A motivação (o log já gravou o QR em texto) está corrigida nativamente:
`src/unlink.rs::link_log` registra só contagem/índice (`a_link_log_never_contains_a_qr_payload`),
o log por execução existe (`AppDirs::log_file`), e alvos de protocolo são
reescritos por `src/diagnostics.rs::protocol_summary`. Restaria só o gancho
de pânico sem conteúdo, que não justifica uma dependência `git`.

## Não adotar agora (já vetado no plano)

`fastframe-i18n` (o Vespera tem os catálogos por idioma, com trava no CI),
`fastframe-theme` (temas próprios), `fastframe-macos`, `fastframe-icons`.
