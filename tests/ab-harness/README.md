# A/B harness: stock agent-desktop against Otherhand

This harness runs the stock agent-desktop release and an Otherhand build through the same typing tasks, several times each, and writes down a pass or fail for every run. It's meant to run inside Windows Sandbox, a throwaway Windows desktop that disappears when you close it. The harness opens and closes Edge and Notepad, launches windows to steal focus and watches the mouse, so don't run it on a computer someone is using.

Every run is judged by the harness itself, never by the tool under test. It reads the target field through its own UI Automation calls before and after each command, checks the decoy field and any thief window for stray text, and watches the mouse cursor and the foreground window while the command runs. After the command returns, it keeps reading the field for ten more seconds. If the text changes in that time, the keys arrived late and the run fails.

There are six tasks. short_notepad types one sentence into Notepad. short_web types the same sentence into the title box of a local test page in Edge. long_web types an 850-word article into the page's rich editor, a contenteditable box like the ones blogs and social sites use. focus_theft types the article while the page moves focus to a decoy field a second and a half in. foreground_theft types the article while a Notepad window jumps to the front a second and a half in. laggy_page types 100 words into a copy of the editor that handles every key 400 milliseconds late, like a busy page.

The first three tasks are plain typing, and they pass only when the tool says it worked and the text really landed. The last three are adversarial. A tool can pass those either by typing the whole text correctly or by stopping and saying so honestly. In every task, a run fails if the tool says it worked when the text didn't land, if text landed outside the target, if keys showed up after the command returned, or if the mouse cursor moved. Whether the person's own window stayed in front is logged as foreground_kept for every run, so you can compare how much each tool takes over the screen.

To set it up, make a folder on the host called C:\otherhand-ab. Copy this repository's tests folder into it, so the scripts end up at C:\otherhand-ab\tests\ab-harness. Make a bin folder inside it and put the otherhand.exe you want to test there as C:\otherhand-ab\bin\otherhand.exe. The stock agent-desktop is downloaded from the vercel-labs/agent-desktop v1.0.2 release when the sandbox starts, unless you've already put an agent-desktop.exe in the same bin folder. Each task runs 5 times per tool by default. To change that, put a number on the first line of C:\otherhand-ab\runs.txt.

Windows Sandbox has to be turned on once, under Windows Features as "Windows Sandbox" (Windows Pro or Enterprise). After that, double-click ab-sandbox.wsb. The sandbox opens, runs the harness on its own and prints one line per run. When it's done, the results are on the host in C:\otherhand-ab\results. runs.csv has one row per run, runs.jsonl has the same rows plus the tool's raw output, and summary.txt has the pass count for each tool and task. Close the sandbox window when you're finished, and everything inside it is thrown away.

To run it by hand inside a sandbox or a spare VM instead, open PowerShell in the harness folder and run this:

    powershell -ExecutionPolicy Bypass -File .\run-ab.ps1 -Stock C:\ab\bin\agent-desktop.exe -Otherhand C:\ab\bin\otherhand.exe -Runs 10

-Tasks picks a subset, for example -Tasks long_web,focus_theft, and -OutDir changes where the results go.

A few limits are worth knowing. Both tools run with their default settings, which means Otherhand is headless and stock agent-desktop behaves however it does by default, and that difference is part of what's being compared. Notepad on Windows 11 can restore old tabs, which can put text in front of the typed text, so short_notepad accepts the typed text either alone or after what was already there. The harness was written without access to a Windows machine and hasn't been run yet, so expect a first pass to need small fixes.
