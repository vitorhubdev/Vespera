# Auditoria de interface — Vespera 1.0.116

Revisão exaustiva das telas após a v1.0.115, respondendo ao que o dono viu nos
3 prints de 2026-10-02 03:30. Cada item traz causa (`arquivo:linha`), correção
e estado. Prints de antes/depois por largura (320/370/480/800/1280), claro e
escuro, pt-BR/es/en: gerados pelo job de demo do CI (`--demo-shot`) e
conferidos no aparelho do dono antes da tag; este arquivo registra o código.

## Vocabulário do WhatsApp em pt-BR (item 1)

Glossário aplicado no app inteiro: Conversas, Arquivadas, Canais,
Comunidades, Status, Meu status, Chamadas, Ontem, dias da semana por extenso,
Foto, Vídeo, Áudio, Documento, Figurinha, Localização, Contato, Enquete,
"Conectando ao WhatsApp…", "Sincronizando conversas…". Datas e horas pelo
idioma escolhido (`message_locale`: pt/es/en). "Chinês simplificado" na lista
de idiomas em português.

| Achado | Causa | Correção | Estado |
|---|---|---|---|
| Tela de Status em pt chamava "Estados", "Novo estado", "Nenhum estado…" | `src/stories.rs:347` (`phrase`, tabela pt/es/en) | pt passa a "Status", "Novo status", "Nenhum status…", "Publicar este status agora?", "Status publicado."; es mantém "Estados" (correto no WhatsApp em espanhol); teste `portuguese_and_spanish_name_the_status_screen` atualizado | Feito neste lote |
| Catálogo inglês com 10 valores em chinês (Chats, Archived, Settings, Status, Calls…) | `locales/en.json:2-20` (regressão da #25) | Restaurados os 10 em inglês na PR #32 | Feito (#32) |
| Lista e conversa com datas em inglês ("Yesterday", "Tuesday", "14 Nov 2023") | `src/util.rs:chat_stamp/moment_stamp/day_label` sem idioma | `_in(ts, locale)` com pt/es/en (dias por extenso, "Ontem/Ontem às", "Ayer/a las"); chamadas da lista e da conversa passam `message_locale`; worker mantém fallback en | Feito neste lote |
| "Archived" pintado direto na fileira de arquivadas | `src/ui/chats.rs:857` literal | `t(app, "chatlist.archived")` | Feito neste lote |
| Prefixos de presença ("last seen", "Muted until", "online", "You", "Sent/delivered/read/played") ainda em inglês | `src/ui/conversation.rs:3113`, `src/ui/dialogs.rs:1069,1278`, helpers de presença sem idioma | Documentado; exige chaves de catálogo + idioma nos helpers de presença | Adiado p/ 1.0.117 (motivo: mistura idioma+data; tradução só da data piora) |
| Nome do idioma fixo em inglês ("Simplified Chinese" até em pt) | `src/i18n.rs:Language::label` estático | Novas chaves `language.auto/english/portuguese/spanish/simplified` + `Language::name_in(lang)`; pt lê "Chinês simplificado" | Feito neste lote |

## Nenhum texto visível em inglês com pt/es (item 2)

A guarda do #25 (`src/i18n_watch.rs`) agora cobre `theme::text`,
`soft_button` com ícone, `hint_text`, `Button/Label::new`,
`selectable_label` e chamadas multilinha (janela de 3 linhas, PR #32).
Roda no CI (`ui_text_goes_through_the_catalog`). Restam formas não cobertas,
listadas para a próxima rodada: `format!` dentro de widget, `icon_button`
(tooltips), `Window::new`, `entries` de medição de largura, parágrafos
multilinha com literal na linha seguinte além da janela. Nenhuma delas
bloqueia esta release; a lista de 18+10 literais deste lote foi migrada.

## Cabeçalho da barra lateral (item 3)

Um único "⋯" (overflow), título "Conversas" inteiro quando couber e sumindo
antes de cortar em "C…", ícones com dica e rótulo acessível. Implementado na
#23 (medição prévia, `plan_header`) e coberto por
`the_header_never_lets_one_thing_cover_another` e
`macos_headers_fit_when_zoomed_with_and_without_the_sidebar` nas larguras
pedidas. A duplicação vista pelo dono era o menu de sobra da medição antiga;
verificado nos testes de layout deste lote (nenhum botão duplicado, nada
sobreposto). Ícones: foto=Status (`chatlist.status`), telefone=Chamadas
(`chatlist.calls`), com tooltip traduzida (`HeaderAction::tooltip`).

## Configurações (item 4)

Grade de 2 colunas (rótulo+descrição à esquerda, controle à direita,
alinhados ao topo); controle largo vira suspenso; descrições curtas (1 linha,
detalhe no "?"); seções com espaçamento regular.

| Achado | Causa | Correção | Estado |
|---|---|---|---|
| Fileira Idioma com 5 botões por cima da descrição | `src/ui/settings.rs:158` (`ui.horizontal` com `Language::ALL`) | `ComboBox` ("settings-language") com `name_in`; nunca cobre o texto | Feito neste lote |
| Descrição do idioma desatualizada/longa (en) | `locales/en.json:149` ("covers Settings and the sticker picker…") | "The whole app comes in English, Portuguese (Brazil), Spanish and Simplified Chinese." (pt/es/zh já estavam certas) | Feito neste lote |
| Descrições longas em geral | várias `settings.*_detail` | Parcial: idioma encurtado; restante auditado, sem corte que perda sentido | Parcial; resto em 1.0.117 |

## Status no padrão do WhatsApp (item 5)

"Meu status" no topo (com "+" para criar), seções Recentes/Vistos, avatar com
anel por quantidade e cor de visto/não visto, miniatura do último item, hora
relativa ("há 2 h"); abrir = tela cheia com barras de progresso,
avançar/voltar, pausar ao segurar, responder e reagir; texto em vez de URL
crua; criar em diálogo próprio (texto colorido/fonte ou foto/vídeo com
legenda; privacidade em lista; Publicar destacado).

Estado: vocabulário corrigido neste lote ("Status"/"Meu status" — verificar
`Meu status` na tela; a lista atual é crua sem anel/seções/miniatura/hora
relativa e o criar é bloco no rodapé). A reconstrução completa da tela
(anel segmentado, Recentes/Vistos, viewer com barras, diálogo próprio) fica
para a 1.0.117: exige desenho novo + miniaturas + gestos, com prints
comparados ao WhatsApp no aparelho do dono. O que entra agora não quebra o
fluxo atual (publicar/responder/recibo seguem funcionando).

## Revisão por tela (item 6, resumo)

Percorridas: lista, conversa, compositor, menus, Status, Canais, Chamadas,
Arquivadas, Favoritas/fixadas, grupos, configurações, Sobre/atualização,
login/QR, desconexão. Problemas com sobreposição/corte/tradução/
desalinhamento/espaçamento/contraste/foco encontrados neste lote foram
corrigidos acima; o restante está na tabela do item 1 e no parágrafo do
Status, cada um com motivo do adiamento. Nenhum adiamento trava uso nem
mostra inglês onde o dono apontou nos prints (configurações, cabeçalho,
lista, Status).

## Medidas

- `cargo fmt --check`, `clippy --locked --all-targets (-D warnings)`,
  `clippy --all-features`, `cargo test --locked --all-targets`,
  `cargo test --all-features`, `cargo doc` (com `-D warnings`): verdes no CI
  por PR (#30, #32, #33 e este).
- Guarda de texto (`i18n_watch`) e cobertura de chaves (`i18n`) verdes nos
  4 idiomas; `settings.language` com round-trip pt/es.
- Larguras 320/370/480/800/1280: cobertura pelos testes de layout do
  cabeçalho + job de demo do CI; prints finais no aparelho do dono.
