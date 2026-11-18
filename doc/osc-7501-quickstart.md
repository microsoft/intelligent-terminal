# See program status in the Intelligent Terminal sidebar

If your program reports status using [OSC 7501 (Program Status Protocol)](https://www.superlogical.com/rex/docs/build/program-status), Intelligent Terminal updates the **Agent status** field for its open tab or pane automatically. For example, when a running tool asks a question, its status can change from **Active** to **Waiting for input**. This works over SSH, too, as long as the reports reach your local terminal. Multiplexers such as tmux or screen may filter them.

**If your program doesn't support OSC 7501**, use shell integration in the shell that runs it. The shell can report when a command starts and when it exits, including its exit code. It can't tell whether the program is halfway through a build or waiting for approval.

**If you write your own script**, you can report those milestones directly. For example, in Bash, emit this just before asking the user a question:

```bash
printf '\033]7501;state=blocked:kind=question\007'
```

Once work resumes, send `state=working`; when it finishes, send `state=done` or `state=error`. The [public specification](https://www.superlogical.com/rex/docs/build/program-status#examples) includes examples with progress, messages, and a reusable shell function.
