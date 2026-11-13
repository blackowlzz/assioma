# Assioma

Hi! Assioma is the code editor I wanted for myself: it looks and feels like VS Code, but it's light, it starts fast, and it's entirely yours. No account, no telemetry, nothing running in the background that you didn't ask for. It's written in Rust and Tauri, and with a project open it uses around 350 MB of memory.

It's still young, so you'll find rough edges. If something breaks or feels wrong, please open an issue and tell me what you were doing. I read all of them.

## Download

Get the latest version from [Releases](https://github.com/blackowlzz/assioma/releases). There's an AppImage, a `.deb` and an `.rpm` for Linux, and an installer for Windows. Assioma doesn't check for updates on its own, so come back to that page now and then.

From a terminal you can open a folder with `assioma .`: turn that on once from Help > Install the assioma Command.

## What you get

**Your languages, colored like in VS Code.** Assioma uses the same TextMate grammars, so C, C++, C#, CSS, Dart, Go, HTML, Java, JavaScript, TypeScript, JSON, Lua, Markdown, PHP, PowerShell, Python, R, Ruby, Rust, Shell, SQL, Swift, XML, YAML, Dockerfile, Makefile and many more look the way you're used to. If a file has an unusual name, pick its language from the status bar.

**Language servers, like in VS Code.** If you have a language server installed (clangd, rust-analyzer, Pyright, gopls, typescript-language-server and many others), Assioma starts it when you open a file of its language. You get errors as you type, go to definition (F12 or Ctrl+click), find all references (Shift+F12), rename (F2), quick fixes (Ctrl+.), formatting (Shift+Alt+F), hovers, parameter hints and smarter completion. The status bar tells you which server is working, and if one is missing Assioma tells you how to install it. Settings > Language Servers lists them all, lets you turn any of them off, or point Assioma to one installed somewhere else.

**Extensions from Open VSX.** Press Ctrl+Shift+X to search [open-vsx.org](https://open-vsx.org), the open marketplace for VS Code extensions, and install color themes, language grammars and snippets. Assioma never runs extension code, which keeps it light and safe, but it also means language servers and debuggers from extensions don't work. The panel tells you when that's the case.

**A layout you arrange yourself.** Every tool window (Project, Commit, Extensions, Find, Run, Terminal, the AI chat) can go in the left side bar, the right one, or the panel at the bottom. Drag its icon, or its tab, where you want it, and Assioma remembers. Right-click an icon to move it from a menu, or to reset everything.

**Split editors.** Ctrl+\ puts two files side by side, each with its own tabs. Drag the line between them to resize, and use Toggle Split Direction if you'd rather have one above the other. Your split comes back the next time you open the project.

**The editing you're used to.** Multiple cursors (Alt+Click, Alt+J for the next match, Ctrl+Shift+L for all of them), word wrap with Alt+Z, folding, bracket matching that ignores brackets in strings and comments, find and replace in the file or in the whole project, a command palette, and themes you can change color by color.

**Run, Git and a terminal.** Shift+F10 runs the file you have open with the right command for its language (`python3`, `node`, `go run`, `cargo run` and so on), and you can change that command per file type. There's a commit panel with branches, merge and stash, and a real terminal.

**An AI that stays on your computer.** Gray suggestions finish the line you're typing, and Tab accepts them. The chat on the right (Alt+8) can see the file of the editor you were last in, and its name is always shown under the message box, so you know what you're sending. Install the small built-in model (Qwen2.5-Coder 0.5B) or the better one (1.5B) from the notification and that's it. If you prefer, connect Ollama, LM Studio, llama.cpp or an online service in Settings > AI. Nothing runs and nothing is sent anywhere until you turn it on.

**It's fast where it matters.** Huge files, minified JSON on a single line, projects with tens of thousands of files: I test all of these, and Assioma keeps up. Scrolling, typing and searching don't stutter.

## Your data stays yours

Assioma has no telemetry and no account. It only goes on the network when you ask it to:

- when you search or install extensions, it talks to Open VSX;
- when you install the built-in AI, it downloads the engine and the model from GitHub and Hugging Face, at fixed versions, and checks every file against a known SHA-256 before using it;
- when you set up your own AI server, it talks to that server and nothing else.

There's also an optional Discord status that shows the file and project you're working on. It only talks to the Discord app on your own computer, and you can turn it off in Settings > General.

## Shortcuts

Mostly the same as VS Code. Ctrl+Shift+A finds any command if you forget one.

| Keys | |
| --- | --- |
| Ctrl+P | Open a file |
| Ctrl+Shift+A | Find any command |
| Ctrl+Shift+F | Search the project |
| Ctrl+Shift+X | Extensions |
| F12, Shift+F12 | Go to definition, find all references |
| F2, Ctrl+. | Rename, quick fix |
| Shift+Alt+F | Format the document |
| Ctrl+Shift+O, Ctrl+T | Go to a symbol in the file, in the project |
| Ctrl+\ | Split the editor |
| Shift+F10, Ctrl+F2 | Run, stop |
| Ctrl+/ | Comment |
| Ctrl+D, Ctrl+Y | Duplicate, delete a line |
| Alt+Shift+Up, Alt+Shift+Down | Move a line |
| Alt+J, Ctrl+Shift+L | Next match, all matches, as more cursors |
| Alt+Z | Word wrap |
| Alt+8 | AI chat |
| Alt+F12 | Terminal |
| Ctrl+` | Switch theme |

## Make it yours

Everything visual is in Settings: themes and every single color, fonts, spacing, the caret, and the small animations. The animations are on by default. If your computer is on the slow side, turn them off in Settings > Interface; if your system asks apps for less motion, Assioma already keeps them off.

## Not there yet

I'd rather tell you now than have you find out:

- Language servers aren't bundled: you install the ones you want, and Assioma finds them.
- There's no debugger.
- Extensions can add themes, languages and snippets, but not code, so language servers come from your system rather than from extensions.
- When the same file is open in both halves of a split, the two share one cursor.

## Building it yourself

You need Rust, `cargo tauri` and the WebKitGTK 4.1 development files.

    cd src-tauri
    cargo run --release -- /path/to/folder

To build an AppImage and install it for your user:

    sh scripts/install-local.sh

The fonts are Inter Tight and JetBrains Mono, both under the SIL Open Font License. Third-party licenses are listed in [THIRD_PARTY.md](THIRD_PARTY.md).

Assioma grew out of [clight](https://github.com/blackowlzz/clight), my C and C++ IDE for low-end machines. If you write mostly C or C++, have a look at it too: it has clangd and a debugger built in.

## License

You can use, copy, change and share it freely, as long as you don't sell it and you keep a credit to clight and Assioma. It comes as is, with no warranty: see [LICENSE](LICENSE) for the exact terms.
