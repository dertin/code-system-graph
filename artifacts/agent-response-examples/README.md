# Ejemplos de respuestas para agentes

Abrir `index.html` en un navegador. Es un documento autónomo y funciona sin conexión.

Contiene 13 llamadas MCP reales a Code System Graph 1.2.0 compilado desde el commit registrado en `responses.json`, usando CodeGraph 1.6.1 real y la fixture sintética `fixtures/platform-demo`. Cada carpeta se preparó como repositorio Git local. No son capturas de repositorios de producción ni respuestas inventadas.

Cada ejemplo muestra la pregunta que motiva la llamada, sus argumentos exactos, `content[].text`, `structuredContent` y el `CallToolResult` completo. Las notas de lectura y los puntos a evaluar están separados de la respuesta original. Los tamaños se calculan en caracteres; el JSON se mide compacto y se muestra indentado. No se presentan como tokens ni como un costo real del host.

Se pueden anotar evaluaciones por caso y descargarlas como JSON. El documento guarda las notas en el navegador cuando este permite almacenamiento local y no las transmite.

## Reproducir

Requisitos: Python 3 con el paquete `markdown`, Git, CodeGraph 1.6.1 y un binario `csgraph` 1.2.0. Compilar Code System Graph desde este checkout con `cargo build --locked -p code-system-graph --bin csgraph`.

```bash
CSGRAPH_EXAMPLES_BIN=/ruta/al/csgraph python3 artifacts/agent-response-examples/capture.py
python3 artifacts/agent-response-examples/build_html.py
```

El capturador prepara copias de la fixture, índices y SQLite en `.work/`, que está excluido de Git. No cambia las fuentes de la fixture original. Los IDs y revisiones dependen de esos checkouts; los argumentos se seleccionan de los nodos realmente exportados. El servidor MCP se cierra al terminar o fallar la captura.

`responses.json` conserva la captura original completa; `build_html.py` añade presentación y notas editoriales. `index.html` contiene ambos embebidos y no necesita archivos externos.

## Revisión de calidad

El [plan de mejora para agentes LLM](../../docs/AGENT_RESPONSE_QUALITY_PLAN.md) revisa los 13 casos, distingue correcciones respaldadas por evidencia de hipótesis y propone evaluaciones locales.

`python3 artifacts/agent-response-examples/audit_capture.py` verifica las capturas embebidas y genera `analysis.json` con tamaños y repetición de objetos. No mide tokens ni calidad de un modelo. Los conteos de repetición de entidades, relaciones y evidencias se solapan y no deben sumarse como ahorro neto.
