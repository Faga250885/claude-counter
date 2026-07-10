![Windows](https://img.shields.io/badge/platform-Windows-blue)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

🇬🇧 [English](README.md) | 🇪🇸 Español

# Claude Counter

Un widget liviano para la barra de tareas de Windows que muestra cuánto te queda de tu ventana de uso de Claude Code, Codex y/o Google Antigravity, sin necesidad de abrir una terminal o el sitio del proveedor.

## Qué incluye

- Una barra **5h** para tu ventana actual de uso de Claude de 5 horas
- Una barra **7d** para tu ventana actual de 7 días
- Una cuenta atrás en vivo junto a cada barra que muestra exactamente cuándo se reinicia el límite (por ejemplo `1h 19m`, `2d`)
- Una pequeña mascota animada: camina más rápido cuanta más cuota te queda, se va haciendo más lenta a medida que te acercás al límite, y se duerme cuando una ventana llega al 100%
- Un widget nativo pequeño que vive directamente en la barra de tareas de Windows, adaptándose automáticamente al tono claro u oscuro real de la barra
- Icono en la bandeja del sistema con un borde de progreso que muestra tu uso
- Barras opcionales de uso de Codex junto a Claude Code
- Barras opcionales de uso de Antigravity para las ventanas de cuota de 5 horas y semanal de Google
- Opciones de clic derecho para actualizar, elegir los modelos mostrados, frecuencia de actualización, idioma, inicio automático y visibilidad del widget
- Ubicación en varios monitores, para que el widget viva en la barra de tareas de la pantalla que prefieras

## Para quién es

Esta app es para usuarios de Windows que ya tienen **Claude Code (CLI o App) instalado y con sesión iniciada**.

El soporte de Codex es opcional. Para mostrar el uso de Codex, instalá e iniciá sesión en la CLI de Codex, y luego habilitá Codex desde el menú de clic derecho **Modelos**.

El soporte de Antigravity también es opcional. Para mostrar el uso de Antigravity, instalá e iniciá sesión en Google Antigravity, y luego habilitá el modelo **Antigravity** desde el menú de clic derecho **Modelos**.

Funciona mejor si querés una pantalla simple de "¿qué tan cerca estoy del límite?" que esté siempre visible.

## Requisitos

- Windows 10 o Windows 11
- Claude Code (CLI o App) instalado y autenticado
- Opcional: CLI de Codex instalada y autenticada, si querés ver el uso de Codex
- Opcional: Google Antigravity instalado y autenticado, si querés ver el uso de Antigravity

Si usás Claude Code a través de WSL, también es compatible. La app puede leer tus credenciales de Claude Code desde Windows o desde tu entorno WSL.

## Instalación

1. Descargá el último `ClaudeCounter-Setup.exe` desde la página de [Releases](../../releases).
2. Ejecutalo. No requiere permisos de administrador.
3. Si es la primera vez que usás Claude Code en esta PC, el instalador te guía para instalar la CLI e iniciar sesión antes de poder terminar.

¿Todavía no tenés la CLI de Claude Code? El paso de configuración del instalador se encarga de eso, pero también podés hacerlo vos mismo en cualquier momento:

```powershell
irm https://claude.ai/install.ps1 | iex
claude
```

Después seguí las indicaciones para iniciar sesión (usá `/login` si no te lo pide automáticamente).

## Uso

Una vez en ejecución, el widget aparece en tu barra de tareas y como uno o más iconos en el área de notificación.

- Arrastrá el icono de la mascota para mover el widget de la barra de tareas
- En configuraciones con varios monitores, arrastrá el widget a otra barra de tareas de Windows para moverlo a esa pantalla
- Hacé clic derecho sobre el widget o el icono de la bandeja para acceder a actualizar, modelos mostrados, frecuencia de actualización, iniciar con Windows, restablecer posición, idioma y salir
- Hacé clic izquierdo en el icono de la bandeja para mostrar u ocultar el widget de la barra de tareas
- Habilitá `Iniciar con Windows` desde el menú de clic derecho si querés que se abra automáticamente al iniciar sesión

### Modelos

Usá el menú de clic derecho **Modelos** para elegir qué muestra el widget:

- **Claude Code** está habilitado por defecto
- **Codex** se puede habilitar junto a Claude Code o mostrarse solo
- **Antigravity** se puede habilitar junto a los otros proveedores o mostrarse solo, como su propia columna de modelo

Cuando se muestran varios modelos, cada uno tiene su propia barra de uso y su color de texto correspondiente.

### Icono de la bandeja del sistema

El icono de la bandeja muestra tu uso actual de 5 horas como un porcentaje, con un borde blanco que va rodeando el perímetro del icono en proporción a tu uso.

Si hay varios proveedores habilitados, la app muestra un icono de bandeja por proveedor. Si solo hay un modelo habilitado, muestra un único icono.

## Diagnóstico

Si necesitás solucionar problemas de inicio o visibilidad, ejecutá el archivo instalado con:

```powershell
claude-code-usage-monitor.exe --diagnose
```

Esto escribe un archivo de registro en:

```text
%TEMP%\claude-code-usage-monitor.log
```

La configuración se guarda en:

```text
%APPDATA%\ClaudeCodeUsageMonitor\settings.json
```

## Privacidad y seguridad

Este proyecto es de **código abierto**, así que podés revisar exactamente qué hace.

Qué lee la app:

- Tus credenciales OAuth locales de Claude Code desde `~/.claude/.credentials.json`
- Si hace falta, el mismo archivo de credenciales dentro de una distro de WSL instalada
- Si Codex está habilitado, tus credenciales locales de Codex desde `$CODEX_HOME/auth.json` o `~/.codex/auth.json`
- Si Antigravity está habilitado, tu token OAuth local de Antigravity desde el Administrador de Credenciales de Windows, en el destino `gemini:antigravity`

Qué envía la app por la red:

- Solicitudes a los endpoints de Claude de Anthropic para leer tu información de uso y límites de tasa
- Solicitudes al endpoint de uso de Codex de ChatGPT para leer tu uso y límites de Codex, si está habilitado
- Solicitudes a los endpoints de Cloud Code / Antigravity de Google para leer tu información de cuota de Antigravity, si está habilitado
- Si hay variables de entorno de proxy como `HTTPS_PROXY`, `HTTP_PROXY` o `ALL_PROXY` configuradas, esas solicitudes salientes pueden usar ese proxy

Qué guarda la app localmente:

- Posición del widget
- Barra de tareas / pantalla seleccionada
- Visibilidad del widget
- Frecuencia de sondeo
- Preferencia de idioma
- Preferencias de modelos mostrados

Qué **no** hace:

- No envía tus credenciales a ningún otro servidor
- No usa un servicio de backend separado
- No recolecta analíticas ni telemetría
- No sube los archivos de tus proyectos
- No edita directamente tu archivo de credenciales de Codex

Notas:

- Si tu token de Claude Code expiró, la app puede pedirle a la CLI local de Claude que lo renueve en segundo plano
- Si tu token de Codex expiró, la app puede pedirle a la CLI local de Codex que lo renueve en segundo plano. La app no escribe `auth.json` por sí misma; cualquier actualización de credenciales la maneja la CLI de Codex.
- Si tu token de Antigravity expiró, abrí Antigravity e iniciá sesión de nuevo. La app no escribe entradas en el Administrador de Credenciales de Windows por sí misma.
- Los proxies deben ser de confianza, porque las solicitudes de uso que pasan por el proxy incluyen tu token OAuth dentro de la conexión TLS

## Cómo funciona

La app:

1. Encuentra las credenciales de inicio de sesión de los modelos habilitados
2. Lee tu uso actual desde los endpoints de Anthropic, ChatGPT y/o Google Antigravity
3. Muestra el resultado directamente en la barra de tareas de Windows
4. Mantiene el widget alineado con la barra de tareas y el área de bandeja seleccionadas
5. Se actualiza periódicamente en segundo plano

Si el endpoint de uso más nuevo no está disponible, puede recurrir a leer los encabezados de límite de tasa que devuelve la API de Mensajes de Claude.

## Código abierto

Este proyecto tiene licencia MIT y está basado en el proyecto de código abierto [Claude-Code-Usage-Monitor](https://github.com/CodeZeno/Claude-Code-Usage-Monitor) de Craig Constable.

Si querés revisar el comportamiento o auditar el código, todo está en este repositorio.
