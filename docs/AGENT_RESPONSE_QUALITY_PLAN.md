# Plan de mejora de respuestas para agentes LLM

Estado: implementación 1.2.1 con adopción del nuevo predeterminado condicionada a evaluación. Fecha: 2026-10-03.

La implementación y sus límites verificables se documentan en [MCP](MCP.md#canonical-response-preview-121) y en [evaluación](../evaluation/agent-responses/README.md). El contrato anterior sigue disponible y es el predeterminado hasta cerrar las puertas de calidad/host descritas abajo.

## 1. Decisión principal

Code System Graph debe optimizar la capacidad del agente de **identificar una relación, comprobar su evidencia y continuar la investigación**, conservando los límites de lo observado. La decisión de diseño es **Markdown autosuficiente como salida por defecto y JSON como formato opcional**. Esto no presupone superioridad universal de Markdown ni una salida ya óptima. El problema actual combina pérdida de información en el texto, duplicación en el JSON y diferencias de contenido entre ambos canales.

Ambos formatos deben ser transformaciones finales de una única respuesta canónica: mismos hechos seleccionados, evidencia, fuente, límites y próximas acciones. El agente debe poder completar la investigación recibiendo únicamente Markdown, sin necesitar un JSON complementario. JSON se conserva para consumidores que lo soliciten; no constituye una segunda fuente de verdad ni un canal donde acumular detalles exclusivos. Las pruebas validarán suficiencia, calidad y costo del diseño elegido. No se propone convertir el grafo en un evaluador de corrección de PRs.

La relación `confirmed` confirma un enlace según las reglas del grafo; no confirma un bug, la vigencia de un contrato ni que una rama externa sea la referencia correcta. La integración con Themis debe mantener esta distinción.

## 2. Evidencia y alcance

Se revisaron individualmente los 13 casos de [la galería](../artifacts/agent-response-examples/index.html), sus argumentos, texto, `structuredContent` y resultado MCP completo, y se contrastaron con la implementación del checkout `32e2cbff3fc245f6ac0191d21755562618f9dce1`.

La captura usa CSG 1.2.0, CodeGraph 1.6.1 y repositorios sintéticos derivados de `fixtures/platform-demo`. No representa todas las herramientas, lenguajes, hosts ni modelos. Las preguntas explicativas de la galería son editoriales: la herramienta recibió los argumentos mostrados, no necesariamente esa pregunta completa.

El [análisis reproducible](../artifacts/agent-response-examples/analysis.json) verifica que las respuestas y argumentos embebidos en HTML coinciden con `responses.json`, mide tamaños y contabiliza objetos repetidos. Se reproduce con:

```bash
python3 artifacts/agent-response-examples/audit_capture.py
```

Los tamaños siguientes son caracteres Unicode; JSON compacto, sin indentación. No son tokens ni una estimación del costo del modelo. El resultado MCP serializado completo suma 118.875 bytes. Sus dos canales contienen 16.095 caracteres de texto y 101.247 de JSON: el JSON representa aproximadamente el 86 % de esa suma, pero eso **no demuestra que el 86 % sea ruido** ni que el host reenvíe ambos canales al modelo.

| Caso | Texto / JSON | Qué aporta y qué cambiar o evaluar |
|---|---:|---|
| `broad` | 3.716 / 25.431 | Descubrimiento de tablas, eventos, infraestructura y documentación. 30 apariciones de entidades para 15 objetos distintos y 11 relaciones para 7 distintas. Reducir repetición; evaluar orden y detalle según intención. No eliminar documentación o infraestructura globalmente. |
| `http` | 2.012 / 19.998 | Recupera endpoint, test y cliente, pero el servicio de infraestructura queda primero y orienta las próximas acciones hacia `infra`. El texto oculta los argumentos de esas acciones. Corregir continuidad; evaluar ranking con consultas de rutas. |
| `http-filtered` | 1.294 / 14.220 | El filtro `http_operation` deja los tres participantes pertinentes y conserva dos enlaces. Hay 15 apariciones de entidades para 3 distintas. Buen candidato para normalización y guía de filtros; no prueba que un filtro automático sea siempre correcto. |
| `test-query` | 1.476 / 8.801 | Encuentra el test y su vínculo con API. El título elimina guiones bajos y se presenta `python/pytest::test_create_order` como ruta. Corregir fidelidad del identificador y origen del localizador. Evaluar resultados secundarios, como la migración. |
| `test-context` | 656 / 2.925 | Respuesta relativamente compacta: test, endpoint y evidencia. Conservar su concisión; dar un identificador utilizable y un localizador válido. La ausencia de código de la aserción es coherente con esta herramienta: debe orientar a `explore`. |
| `trace` | 383 / 2.153 | Camino de una arista con dirección y ambos repositorios. Usar como referencia de concisión. Corregir gramática del singular y evaluar omisión de indicadores falsos redundantes en una nueva proyección. |
| `events` | 1.806 / 18.232 | Conecta publicación API con consumo worker. El texto muestra dos evidencias de API y omite la del consumidor que sí está en JSON. Priorizar evidencia de ambos extremos. La coincidencia con `orders.created_at` y los canales homónimos requieren evaluación de ranking e identidad. |
| `database` | 1.042 / 4.458 | Diferencia lectura observada, propiedad estructural del esquema y documentación. Conservar esa distinción: definir una tabla no demuestra acceso en ejecución. Hacer visibles los roles de evidencia; evaluar cuánto contexto documental mostrar por defecto. |
| `web-explore` | 731 / 939 | Devuelve código de `createOrder` con `fetch`, pero ningún handoff aunque la consulta federada sí encuentra la relación web→API. Investigar y corregir correlación precisa entre símbolo y llamada; no usar coincidencia de archivo como sustituto. |
| `api-explore` | 1.433 / 1.212 | Devuelve handler y contexto local; el Markdown incluye código y el JSON no. Que JSON sea menor aquí no es una victoria de formato. Evaluar recorte semántico del archivo completo sin perder ruta, anotaciones o contexto de retorno. |
| `test-explore` | 1.024 / 1.990 | La aserción `201` está en el código del texto; IDs, revisión y argumentos de continuidad están en JSON. Un agente que reciba solo uno de los canales pierde información útil. Corregir equivalencia semántica y acciones ejecutables. |
| `missing` | 320 / 653 | Comunica correctamente que no hubo coincidencias y que la búsqueda no equivale a buscar todo el código. JSON mezcla resultado vacío con “Full-text scores were not provided.” y marca degradación. Distinguir búsqueda ejecutada sin resultados de búsqueda no disponible. |
| `bad-scope` | 202 / 235 | Error breve y válido al faltar repositorio en `explore`. Añadir recuperación concreta: aliases acotados o acción de descubrimiento. No elegir un repositorio arbitrariamente. |

El filtro HTTP redujo los caracteres combinados un 29,5 % frente a la consulta sin filtro. Es una observación del caso, no una mejora de precisión medida con un LLM.

## 3. Qué cambiar con evidencia suficiente

### P0 — Fidelidad, evidencia y continuidad

**A. Preservar nombres y separar nombre de ruta.**

- Sustituir la eliminación de caracteres de `heading_text` por escape o presentación literal segura. `test_create_order` debe seguir siendo `test_create_order`.
- Obtener `path` de un localizador de fuente tipado o evidencia válida. No deducirlo de una etiqueta por contener `/`.
- Mantener `qualified_name` separado. Si no existe un archivo verificable, representar la ausencia; no fabricar una ruta navegable.
- Aceptación: nombres exactos, rutas existentes en la revisión correspondiente y líneas válidas en todos los casos; pruebas con caracteres Markdown y contenido adversarial tratado como datos.

**B. Hacer ejecutable la próxima acción en la presentación para el agente.**

- Mostrar nombre de herramienta y argumentos mínimos exactos: `repository`, `node_id`, consulta y límites cuando correspondan.
- Incluir los IDs necesarios para continuar, una sola vez por entidad o acción. No repetir todos los identificadores internos de aristas y evidencias.
- No limitarse a “Use source_context”: la acción debe poder copiarse o traducirse a una llamada sin adivinar IDs.
- Mantener los argumentos estructurados para clientes. En errores de alcance, dar aliases acotados o una acción válida para obtenerlos.
- Aceptación: ejecutar contra la misma captura viva todas las continuaciones sugeridas; cero IDs inexistentes y cero argumentos incompatibles con el esquema. La validez sintáctica no basta: la acción debe corresponder a la entidad indicada.

**C. Seleccionar evidencia que explique ambos extremos.**

- Reservar evidencia del origen y del destino antes de aplicar el límite de una relación entre repositorios.
- Mostrar rol y procedencia: llamada observada, publicación, suscripción, validación, documentación o atribución estructural.
- Si no hay evidencia de un extremo, hacerlo explícito. No inventarla para completar la presentación.
- Para eventos, explicar la derivación publisher→canal→subscriber. No presentarla como llamada directa ni como entrega garantizada en ejecución.
- Aceptación: `events` debe incluir evidencia de `worker/worker.py:5` junto con evidencia de API; `database` debe conservar lectura observada frente a propiedad del esquema.

**D. Recuperar handoffs cuando la llamada está dentro del símbolo.**

En `explore/correlation.rs`, la correlación por evidencia comprueba si el inicio del símbolo cae dentro del rango de evidencia. En el caso web, el símbolo comienza en línea 1 y la llamada está en líneas 2–6. El código y la captura respaldan esta explicación, que debe cerrarse con una regresión reproducible antes de modificar el algoritmo.

- Preferir identidad del símbolo contenedor o rangos públicos precisos del proveedor para asociar el callsite.
- Si falta esa información, distinguir “no se pudo correlacionar” de “no hay relaciones”.
- No ampliar a cualquier relación del mismo archivo. Mantener aislamiento por repo, revisión, símbolo y dirección.
- Aceptación: web obtiene el handoff esperado; dos funciones del mismo archivo, funciones anidadas, rutas ambiguas y archivos homónimos de distintos repos no reciben relaciones ajenas. No depender de tablas internas de CodeGraph.

**E. Separar vacío, incompleto y error.**

- Modelar si FTS se ejecutó correctamente, no se ejecutó o falló; un mapa vacío de puntuaciones no distingue estos estados.
- Revisar `search_coverage` y sus llamadores antes de cambiar el estado de `missing`. No convertir automáticamente todo resultado vacío en `ok`.
- Mantener estado, frescura, advertencias y cobertura como dimensiones distintas.
- Mostrar causas accionables de obsolescencia y repos afectados cuando existan, sin imprimir bloques de diagnóstico vacíos en cada respuesta.
- Aceptación: búsqueda válida vacía, proveedor ausente, índice obsoleto, ejecución parcial y fallo real tienen señales diferentes y coherentes en ambos canales.

### P1 — Proyección compacta y límites de entrega

**F. Construir una selección semántica común.**

Introducir una proyección de respuesta para agentes anterior a los renderizadores. Debe contener intención de la operación, hechos seleccionados, entidades referenciadas, relaciones, evidencia, localizadores, límites y continuaciones. Markdown y JSON derivados de esa proyección deben conservar la misma información necesaria para resolver la tarea.

`explore` requiere tratamiento explícito del código fuente: hoy `source_markdown` se entrega solo en texto. Los segmentos de fuente deben formar parte de la respuesta canónica y aparecer en ambos formatos; JSON no puede perder la aserción del test ni sustituir por una llamada adicional el código que Markdown ya entrega. Esto no autoriza a añadir código a todas las herramientas: `query`, `trace` y `source_context` pueden seguir orientadas a navegación y evidencia.

Conservar los informes internos completos. Compactar una vista para el agente no significa borrar información del grafo ni degradar los consumidores de la API existente.

**Arquitectura obligatoria: una fuente de verdad y formatos al final.**

```text
Datos y evidencias del snapshot + solicitud
                    ↓
Resolución, ranking, selección y presupuesto comunes
                    ↓
Respuesta canónica para el agente
            ↙                       ↘
Renderizador Markdown          Serializador JSON
(salida por defecto)           (opción explícita)
```

- La respuesta canónica incluye toda la información útil seleccionada, incluidos segmentos de código de `explore`, roles de evidencia, argumentos de acciones y señales de incertidumbre.
- Los renderizadores reciben esa misma respuesta. No consultan el grafo o proveedores, no hacen ranking, no seleccionan evidencias ni agregan, infieren o descartan hechos por su cuenta. Solo cambian la representación: sintaxis, escape, estructura y redacción determinista.
- Si el tamaño serializado exige reducir contenido, la capa común ajusta la selección y produce otra respuesta canónica con omisiones explícitas. El renderizador no trunca contenido unilateralmente. Con igual solicitud, perfil y presupuesto semántico, ambos formatos representan los mismos hechos.
- Un dato entra en la respuesta cuando ayuda a responder, sustentar, interpretar correctamente o continuar la consulta. Los datos internos sin esa función quedan en diagnóstico separado, no se agregan únicamente a JSON.
- Si se ofrece un perfil de detalle, se aplica antes de elegir el formato y queda disponible por igual para ambos. `format=json` no significa “más información”.
- El modo predeterminado entrega solo el contenido Markdown al agente. La envoltura JSON-RPC de MCP sigue siendo transporte: no implica enviar una segunda respuesta JSON semántica al contexto del modelo.
- Aceptación: pruebas de paridad semántica sobre una misma respuesta canónica, incluidas fuente, acciones, límites y evidencia; pruebas de que ninguno de los renderizadores accede a proveedores o decide contenido. Los tests de equivalencia no deben comparar solo tamaños ni snapshots visuales.


**G. Reducir duplicación, con contrato explícito.**

- En la nueva proyección, representar una entidad y una relación una vez; resultados y caminos pueden referenciarlas.
- Evitar repetir la misma relación en `cross_repository_relations` y dentro de cada `relation_previews`, salvo que el formato elegido lo necesite y la evaluación demuestre beneficio.
- Suprimir prosa repetida como “Its semantic relationships are summarized above” cuando una sección común ya establece ese hecho.
- Omitir vacíos y valores por defecto solo si su semántica está documentada. “Sin evaluar” no puede convertirse en “sin problemas”.
- La forma concreta de referencias frente a objetos anidados queda sujeta a evaluación: ahorrar bytes puede aumentar el trabajo de asociación para el LLM.
- Aceptación mínima: reconstrucción de los mismos hechos seleccionados, referencias válidas y ausencia de pérdida de evidencia o incertidumbre. El ahorro neto se mide incluyendo el catálogo y sus referencias.

**H. Presupuestar la respuesta completa.**

Actualmente el límite de presentación se aplica al Markdown y no acota de la misma forma `structuredContent`. Añadir un presupuesto explícito de entrega completa; no cambiar silenciosamente el significado de la configuración existente.

- Seleccionar hechos antes de renderizar ambos canales. No cortar JSON serializado ni dejar referencias huérfanas.
- Reservar espacio para estado, evidencia mínima y próxima acción. Una relación seleccionada debe conservar extremos y explicación suficiente.
- Informar qué se omitió, por qué y cómo continuar. Mantener paginación correcta cuando hay colapso de resultados o deduplicación.
- Separar límite de transporte, límite de fuente y presupuesto del contexto del host. Los bytes del servidor no equivalen a tokens del modelo.
- Ampliar el test actual de cinco servicios bajo 10 KiB: no representa el caso HTTP real con evidencia y relaciones, cuyo resultado completo ocupa 22.154 bytes.
- Aceptación: respuesta completa dentro del presupuesto configurado o error explícito si el mínimo obligatorio no cabe; nunca truncación semántica silenciosa.

**I. Identidad reproducible del contexto.**

Conservar revisión y frescura cuando están disponibles y establecer una referencia compacta al snapshot para las operaciones que hoy no la muestran. Cuando el agente compara repos, debe poder saber qué revisiones sustentan el enlace. Evitar repetir todos los repos del workspace en cada respuesta: mostrar los implicados o una referencia resoluble al snapshot.

Una rama configurada no prueba compatibilidad. Themis debe conservar su responsabilidad de seleccionar refs y fijar SHAs; CSG debe comunicar la procedencia y los límites del contexto consultado.

## 4. Política de campos

La clasificación se refiere al contexto habitual del LLM, no a eliminación del almacenamiento ni del contrato público actual.

| Campo o familia | Política propuesta | Motivo y condición |
|---|---|---|
| Etiqueta, tipo, alias de repo | Conservar | Identifican participantes; alias solo no sustituye identidad inequívoca. |
| `node_id` | Conservar donde habilite navegación | ID exacto en acciones o catálogo; una aparición suele bastar. |
| `stable_key`, `repository_id`, IDs de evidencia/arista | Disponibles en datos completos; seleccionar | Útiles para integración, identidad y auditoría; no todos deben repetirse en cada relación textual. |
| Ruta, líneas, nombre cualificado | Conservar por separado | Son evidencia y navegación; no intercambiables. |
| SHA/revisión, snapshot, frescura | Conservar contexto relevante | Evitan comparar evidencia sin saber su procedencia. |
| Tipo de arista, extremos y dirección | Conservar | Es el significado esencial de la vinculación. |
| `relationship`, `inverse_relationship`, enum y frase derivada | Evitar expresar lo mismo varias veces | Mantener una representación canónica y renderizarla; probar que no se invierte la relación. |
| `scope` | Seleccionar o derivar con cuidado | Puede ser redundante con repos, pero las atribuciones ambiguas impiden inferencias simplistas. |
| `status`, derivación, atribución y candidatos | Conservar incertidumbre relevante | No ocultar ambigüedad ni confundir propiedad estructural con conducta observada. |
| Evidencia, rol, explicación y procedencia | Conservar selección suficiente | La explicación no reemplaza la localización verificable. |
| `confidence` numérica | Evaluar presentación | No está demostrado que sea probabilidad calibrada. Nunca interpretar `1.0` como certeza de bug. Mantener estados y evidencia aunque se oculte el número. |
| `score`, muchos decimales, `matched_because` | Resumen útil; detalle de ranking bajo demanda | El orden y una razón discriminante suelen servir más que un flotante; confirmar con evaluación. |
| `alternate_node_ids`, `roles` | Conservar acceso; seleccionar | Importan al colapsar representaciones; no descartarlos si cambian identidad o rol. |
| Conteos, offset, límite, truncación, gaps | Conservar lo necesario para cobertura y continuación | No afirmar completitud a partir de una lista parcial. Los conteos internos pueden ir a diagnóstico. |
| `next_actions` | Conservar herramienta, argumentos y motivo breve | Información operativa esencial hoy parcialmente perdida en Markdown. |
| Código de `explore` | Conservar según tarea | Mismo contenido semántico en comparaciones de formato; no confundir ausencia de código con compresión. |
| Ruta absoluta del checkout | Omitir de repetición habitual; mantener descubrible | Puede servir al host que abre archivos; alias+ruta relativa suelen bastar para razonar. Validar clientes antes de retirarla de una vista. |
| IDs locales del proveedor y estadísticas de ejecución | Diagnóstico por defecto | Sirven a integración y rendimiento; normalmente no explican relaciones. No ocultar fallos o ejecución parcial. |
| Listas vacías y booleanos falsos | Omitir solo con valores por defecto inequívocos | Ausente, vacío, desconocido y no evaluado deben seguir distinguiéndose. |

## 5. Markdown, JSON y el host MCP

La especificación MCP admite contenido estructurado y no estructurado. Recomienda además serializar el contenido estructurado en un bloque de texto por compatibilidad; `outputSchema` es opcional y, si se declara, el resultado debe cumplirlo. No establece que Markdown sea superior para un LLM. Véase [MCP, herramientas, versión 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/server/tools).

Decisiones de entrega:

1. Markdown autosuficiente es el formato predeterminado. La implementación debe incorporar desde la respuesta canónica los IDs, argumentos y evidencias que hoy faltan; ocultar simplemente el JSON actual no cumple el objetivo.
2. JSON es una alternativa explícita para integraciones, obtenida de la misma respuesta canónica y con el mismo alcance de información. No enviar ambos formatos al LLM por defecto. Si un cliente requiere ambos por compatibilidad, documentar esa excepción y verificar qué reenvía realmente al modelo.
3. Adaptar declaración y configuración MCP al modo elegido. Un modo solo Markdown no debe anunciar un `outputSchema` que obligue a devolver `structuredContent`. El modo estructurado debe cumplir el contrato y la recomendación de compatibilidad aplicable. Verificar capacidades al iniciar la sesión; evitar cambiar silenciosamente el esquema de una herramienta entre llamadas.
4. Versionar la transición para consumidores existentes. El contrato anterior puede seguir disponible de forma explícita durante la migración; la nueva ruta debe usar selección común, no perpetuar dos lógicas independientes de datos.
5. Medir el mensaje final enviado al modelo usando la fixture sintética. El criterio principal es que Markdown por sí solo permita resolver la tarea y continuar; JSON sirve de control de paridad y de alternativa de integración.
6. Comparar formatos con los mismos hechos, fuente, advertencias y acciones para identificar regresiones. Estas pruebas validan y afinan la decisión de Markdown por defecto; no dejan pendiente esa decisión de producto.

Un formato híbrido candidato es prosa breve para relaciones y fuente, más acciones con argumentos exactos. No fijar todavía que esas acciones sean bloques JSON: los tests actuales prohíben ese bloque en Markdown. Si se elige, actualizar el contrato y los tests deliberadamente; otra opción es una línea literal de llamada con argumentos.

## 6. Decisiones que requieren evaluación

| Hipótesis | Prueba local | Adoptar solamente si… |
|---|---|---|
| Markdown por defecto es suficiente y eficiente | Ejecutar tareas recibiendo únicamente Markdown; JSON equivalente como control | Se cumplen resolución, evidencia y continuidad sin consultar un JSON complementario; corregir omisiones en la capa común. |
| Un catálogo con referencias supera objetos anidados | Dos JSON equivalentes; tareas que siguen 2–4 saltos | Reduce tokens sin aumentar confusión de entidades, direcciones o repo. |
| Favorecer endpoints mejora consultas de rutas | Corpus de rutas, nombres de servicios y preguntas de arquitectura; comparar ranking y próxima acción | Mejora intención endpoint sin degradar consultas legítimas de infraestructura. No ajustar pesos sobre un caso. |
| Filtrar tipos automáticamente reduce ruido | Consultas con intención clara y ambigua; baseline sin filtro | Conserva recall de los participantes necesarios; ante ambigüedad ofrece el filtro en vez de imponerlo. |
| Menos documentación por defecto mejora revisión | Tareas de contrato documentado y tareas de ejecución | No pierde el único contrato disponible ni presenta documentación como comportamiento observado. |
| Mostrar solo confianza categórica basta | Ablación número/estado/evidencia y casos ambiguos | No empeora decisiones calibradas; si se conserva número, su significado se explica sin prometer probabilidad. |
| Fragmentos de fuente superan archivo completo | Recorte por símbolo con imports, decoradores, registro de rutas y contexto dependiente | Conserva evidencia necesaria; reduce tokens totales, incluyendo llamadas extra para recuperar contexto. |
| IDs cortos ahorran contexto | Medir primero costo de IDs; prototipo con ámbito y resolución explícitos | Ahorro material y cero colisiones, handles caducados o referencias entre snapshots. No introducirlos antes de medir. |
| Recursos bajo demanda mejoran eficiencia | Comparar respuesta autocontenida con recuperación posterior real del host | El ahorro compensa rondas extra y no reduce completitud ni accesibilidad. |
| Un límite predeterminado menor es suficiente | Matriz de tamaños y tareas; medir éxito frente a truncación | No empeora cobertura de evidencia ni provoca exploración adicional más costosa. |

Si una prueba es inconclusa, mantener el comportamiento compatible y documentar la incertidumbre. Un tamaño menor no es por sí solo criterio de adopción.

## 7. Evaluación local reproducible

### 7.1 Corpus y verdad esperada

Congelar la captura actual como baseline; guardar commit, versiones, configuración, hashes y revisiones de las fixtures. Añadir un manifiesto por caso con entidades esperadas, relaciones dirigidas, localizadores, incertidumbres, próximas acciones válidas y conclusiones prohibidas.

Expandir a un piloto de al menos 40 tareas que incluya:

- HTTP: servicio, endpoint, cliente, test y rutas iguales en servicios distintos.
- Eventos homónimos, namespaces distintos y evidencia incompleta de publisher/subscriber.
- Lectura/escritura de datos frente a propiedad estructural y documentación.
- Símbolos con llamadas internas, dos funciones en un archivo y funciones anidadas.
- Repos ausentes, revisión distinta, índice obsoleto, operación parcial y consulta válida vacía.
- Paginación, deduplicación y presupuestos que obliguen a omitir resultados.
- Identificadores con puntuación y contenido de repositorio con instrucciones que deben tratarse como datos.
- Herramientas no cubiertas por estos 13 ejemplos, especialmente impacto, contratos y cambios, antes de aplicar una política global de campos.

El ejemplo del test exige `201`, mientras el handler mostrado devuelve una cadena. Esto orienta una investigación; no prueba por sí solo cuál debe corregirse ni autoriza inferir un código HTTP sin verificar la semántica del framework y el contrato aplicable. El manifiesto debe puntuar positivamente reconocer esa limitación.

### 7.2 Capa determinista: sin modelo

Implementar un runner de fixtures que inicie MCP local, ejecute casos y guarde ambos canales y el resultado completo. Propuesta de archivos futuros: `evaluation/agent-responses/cases.json`, `run_capture.py`, `check_invariants.py` y un directorio de resultados fuera de las fuentes del producto.

Comprobar:

- Fidelidad de nombres, identidad por repo/revisión y validez de rutas y rangos.
- Extremos, dirección, rol de evidencia y derivación de cada relación.
- Referencias resolubles y acciones realmente ejecutables en el snapshot capturado.
- Equivalencia de hechos seleccionados entre formatos, incluida la fuente de `explore`, renderizando la misma respuesta canónica sin consultas adicionales ni selección de datos dentro de los renderizadores.
- Suficiencia de Markdown por sí solo y ausencia de un JSON semántico duplicado en el mensaje predeterminado al modelo.
- Diferencias explícitas entre vacío, desconocido, obsoleto, omitido y fallo.
- Respeto del presupuesto completo, cobertura de truncación y paginación sin pérdida ni duplicación inesperada.

No usar igualdad literal de Markdown como única prueba: el objetivo es conservar significado y continuidad. Mantener algunos goldens para verificar legibilidad y contratos públicos.

### 7.3 Capa host y tokenización

Trazar `CallToolResult → adaptador → mensaje de herramienta → solicitud al modelo`. Medir bytes y tokens reales de cada etapa con el tokenizer de la versión evaluada; no usar caracteres/4 como métrica de aceptación.

Verificar si el host duplica los canales, si conserva `structuredContent`, si incluye la fuente y cómo trata errores, recursos y esquemas. Registrar también los tokens totales de la tarea y el número de llamadas: una primera respuesta corta puede causar más consumo al completar la investigación.

### 7.4 Capa LLM

Ejecutar un harness local contra un modelo disponible localmente, por ejemplo mediante una API en localhost. Fijar modelo, versión, tokenizer, prompt, temperatura, herramientas habilitadas, presupuesto y orden de casos. Un harness local que llama a un proveedor remoto no es inferencia local; esa validación debe declararse aparte y no es requisito para las pruebas deterministas.

Brazos iniciales:

- A: respuesta actual tal como la entrega el host elegido.
- B: nueva respuesta canónica en Markdown, única salida al modelo y modo predeterminado objetivo.
- C: exactamente la misma respuesta canónica en JSON compacto, como alternativa explícita y control de paridad.
- D: ambos formatos únicamente como control diagnóstico de duplicación o compatibilidad; no como modo predeterminado propuesto.

La comparación B/C aísla formato; A/B incluye cambios de contenido y no debe atribuirse solo a Markdown. Después hacer ablaciones de catálogo de referencias, confianza numérica, ranking y recorte de fuente, una modificación por comparación.

Comenzar con los 13 casos y tres repeticiones por brazo como prueba de funcionamiento. Luego usar el corpus ampliado con al menos cinco repeticiones por tarea; evaluar también el modelo de Themis si está disponible bajo las mismas condiciones. No considerar las repeticiones de una tarea como tareas independientes al calcular incertidumbre.

### 7.5 Métricas y decisión

| Métrica | Qué determina |
|---|---|
| Éxito de tarea con rúbrica verificable | Si el agente identifica y comprueba la relación solicitada. |
| Corrección de repo, entidad y dirección | Si comprende la vinculación multirepo. |
| Cobertura de evidencia de ambos extremos | Si puede sustentar la relación sin inventar el participante faltante. |
| Continuaciones válidas y éxito tras 2–4 saltos | Si la respuesta sirve para actuar, no solo para leer. |
| Afirmaciones no sustentadas | Si convierte ambigüedad, frescura o enlaces en bugs confirmados. |
| Tokens totales, llamadas y latencia p50/p95 | Costo real de completar la tarea, no solo tamaño de una respuesta. |
| Truncaciones y recuperación de contenido omitido | Si el presupuesto compromete la utilidad. |

Usar verificadores deterministas para IDs, evidencias y llamadas; revisión humana ciega de una muestra y de todos los desacuerdos para conclusiones semánticas. Un LLM juez puede asistir, pero no ser la única fuente de verdad.

Puertas propuestas:

- Invariantes de identidad, dirección, fidelidad y recuperación: 100 % en el corpus determinista; ninguna nueva referencia inválida o atribución falsa.
- Cero pérdida de las advertencias obligatorias y de la evidencia mínima en relaciones seleccionadas.
- Objetivo inicial de eficiencia: al menos 20 % menos tokens totales en tareas comparables, sin deterioro de éxito ni aumento de afirmaciones no sustentadas. Es una meta experimental, no ahorro prometido.
- Comparar resultados pareados por tarea y reportar intervalos de confianza. Una posible tolerancia de no inferioridad de 2 puntos porcentuales requiere dimensionar la muestra; el piloto de 40 tareas no permite asegurar por sí solo ese margen. Ampliar el corpus cuando el intervalo no resuelva la decisión.
- Si falta acceso al modelo objetivo, declarar validación determinista completa y validación de calidad LLM pendiente. Mantener la validación LLM como puerta pendiente para la adopción del nuevo modo predeterminado; no confundir la decisión de diseño con una validación ya realizada.

## 8. Mapa de implementación y compatibilidad

| Área | Archivos principales | Trabajo |
|---|---|---|
| Proyección y campos | `crates/code-system-graph-cli/src/mcp_support/agent_views.rs` | Localizadores, selección común, referencias, versiones y datos disponibles. |
| Presentación y entrega | `crates/code-system-graph-cli/src/mcp_support/presentation.rs` | Nombres, próximas acciones, señales de estado y presupuesto completo. |
| Relaciones y evidencia | `crates/code-system-graph-cli/src/mcp_support/presentation/relations.rs` | Selección bilateral, roles y explicación de derivación. |
| Fuente y handoffs | `crates/code-system-graph-cli/src/mcp_support/presentation/operations.rs` | Código, continuidad y contexto de `explore`. |
| Correlación | `crates/code-system-graph-cli/src/explore/correlation.rs` | Asociación precisa callsite/símbolo y casos negativos. |
| Búsqueda y cobertura | `crates/code-system-graph-core/src/query.rs` y llamadores | Estado de ejecución FTS; ranking solo tras evaluación. |
| Regresiones | `crates/code-system-graph-cli/src/mcp_support/presentation_goldens.rs` y submódulos | Casos reales con enlaces, presupuesto serializado y contrato de presentación. |
| Contrato y documentación | `docs/MCP.md`, `docs/INTERFACES.md`, ADR de interfaces | Explicar canales, equivalencia, límites, versiones y recuperación. |

El esquema de vistas para agentes observado es versión 5; el esquema del dominio/CLI es independiente. Una proyección incompatible debe tener una versión explícita nueva —por ejemplo, 6 tras acordar el contrato— y una ruta de compatibilidad. No incrementar ni cambiar el esquema persistido por una decisión de presentación.

No retirar campos de consumidores existentes sin inventario y prueba de compatibilidad. Corregir también la descripción de “mirror” completo en documentación: hoy el JSON de `explore` no replica el código fuente del texto.

## 9. Orden de ejecución y entregables

1. **Baseline y rúbrica.** Conservar esta captura, agregar expectativas y registrar el comportamiento del host. Salida: corpus versionado, métricas actuales y matriz de campos/canales.
2. **Correcciones P0.** Fidelidad, localizadores, evidencia bilateral, acciones, cobertura y correlación. Cambios pequeños con regresiones positivas y negativas. Salida: capturas comparables y todos los invariantes satisfechos.
3. **Respuesta canónica y presupuesto P1.** Selección común, renderizadores finales sin lógica de datos, contrato versionado, deduplicación candidata y límites completos. Salida: Markdown autosuficiente y JSON opcional semánticamente equivalente; mantener el contrato previo disponible explícitamente durante la migración.
4. **Experimentos.** A/B de formato y ablaciones; ranking, fuente e IDs cortos solo si los datos justifican intervenir. Salida: tabla de calidad/costo con incertidumbre y decisión por hipótesis.
5. **Adopción.** Actualizar documentación y adaptador de Themis tras cumplir las puertas de validación. Salida: Markdown por defecto, JSON opcional, ausencia de duplicación en el contexto, regresiones ampliadas y posibilidad de volver al contrato anterior.

Para cada fase ejecutar pruebas dirigidas de las unidades afectadas y después los checks requeridos por el repositorio. No confundir capturas, tests de presentación y pruebas con LLM: son niveles de evidencia distintos.

## 10. Qué conservar y qué no concluir todavía

Conservar la separación entre navegación federada y lectura de código, la trazabilidad por repo, evidencia localizable, atribución estructural explícita, estados de incertidumbre y alcance obligatorio de `explore`. El caso `trace` demuestra que una respuesta corta puede ser útil sin esconder el vínculo.

No añadir juicios de “test incorrecto” o “endpoint incorrecto” a CSG a partir de un enlace. En Themis, las discrepancias y posibles cambios incompatibles siguen siendo observaciones contextuales con labels de Conventional Comments, no issues confirmados ni instrucciones automáticas de corrección por defecto.

Queda decidido Markdown autosuficiente por defecto, JSON opcional y una única fuente de verdad previa a ambos formatos. No se afirma que Markdown sea universalmente superior. Siguen pendientes de evaluación los filtros automáticos, la reducción general de documentación, los IDs abreviados y los umbrales de confianza. La mejora prioritaria es entregar hechos precisos, suficientes y navegables, sin información exclusiva de un formato ni duplicación obligatoria en el contexto del agente.
