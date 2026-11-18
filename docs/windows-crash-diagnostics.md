# Windows 崩溃诊断

GameL10n 意外退出时，记下崩溃时间和退出前正在执行的操作，用它们在出问题的电脑上查找对应的转储与系统事件。

## 查找崩溃转储

用运行 GameL10n 的 Windows 账号按 `Win + R`，输入 `%LOCALAPPDATA%\CrashDumps`，查找崩溃时间附近的 `GameL10n.exe.<进程 ID>.dmp`。如果 Windows 错误报告（WER）配置了 `DumpFolder`，到该路径查找。直接运行开发构建或重命名程序时，按实际的可执行文件名查找。

## 查找系统事件

1. 按 `Win + R`，输入 `eventvwr.msc`，打开 **Windows 日志 → 应用程序**。
2. 查看崩溃时间附近来源为 **Application Error**、事件 ID 为 `1000` 的记录。确认记录中的出错应用程序名称和路径对应本次运行。
3. 保留事件时间、异常代码、出错模块、故障偏移和程序版本。没有转储时也可以提供这条事件的详情。

## 为下次复现启用转储

配置 WER 本地转储需要管理员权限。要为发行包中的 `GameL10n.exe` 单独启用小型转储，在管理员 PowerShell 中执行下列命令，再复现问题。目录由 WER 的 `DumpFolder` 设置决定，默认是当前用户的 `%LOCALAPPDATA%\CrashDumps`。[微软本地转储文档](https://learn.microsoft.com/windows/win32/wer/collecting-user-mode-dumps)

```powershell
reg add "HKLM\SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\GameL10n.exe" /v DumpType /t REG_DWORD /d 1 /f
```

反馈时附上崩溃前的操作、所用版本和事件详情。有转储时，可通过私下渠道提供，因为转储可能包含 API 密钥或项目内容。维护者可用同一次构建的程序和调试符号定位调用栈。
