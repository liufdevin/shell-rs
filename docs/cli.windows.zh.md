# Windows CLI 使用说明

`ShellLF.exe` 是桌面程序，`shelllf-cli.exe` 是命令行程序。CLI 不会打开另一套主机库，而是通过本机管道请求正在运行的 ShellLF 操作主机；密码和私钥仍由桌面程序管理。

## 首次使用

1. 将 `shelllf-cli.exe` 放在 `ShellLF.exe` 旁边。
2. 打开 ShellLF，添加需要连接的 SSH 主机并完成首次连接，确认主机密钥和认证方式可用。
3. 在「设置 → 外部 CLI」中打开「启用外部 CLI」。
4. 在两个程序所在的文件夹打开 PowerShell。直接用下面的命令，不必先配置 PATH：

```powershell
.\shelllf-cli.exe --help
.\shelllf-cli.exe hosts list
```

每台主机有一个 16 位 ID，使用列表中的 ID，也可以在主机的右键菜单中选择「复制 ID」。不要把名称或地址当作 ID。

## 执行命令

把示例中的 `主机ID` 换成自己的主机 ID：

```powershell
.\shelllf-cli.exe exec 主机ID 'uname -a'
.\shelllf-cli.exe exec 主机ID 'df -h'
.\shelllf-cli.exe exec 主机ID 'tail -n 100 /var/log/syslog'
```

默认会新建临时 SSH 连接，命令结束后断开，退出码是远程命令的退出码。此方式没有交互式标准输入，不要用来运行持续等待输入的程序。

堡垒机不允许重复登录时，可使用已打开的终端：

```powershell
.\shelllf-cli.exe exec --terminal 主机ID 'pwd'
```

**注意：`--terminal` 会先发送 Ctrl+C，打断这个终端中正在运行的命令。** 只应在确认可以打断时使用，支持 bash、dash 和 busybox ash。

## 上传与下载

```powershell
.\shelllf-cli.exe upload 主机ID 'C:\Logs\test.log' '/tmp/'
.\shelllf-cli.exe download 主机ID '/var/log/syslog' 'C:\Logs\syslog'
.\shelllf-cli.exe sync 主机ID 'C:\project\dist' '/srv/app'
```

上传目录会递归复制，已有文件可能被覆盖；执行传输前先确认本地和远程路径。

## 脚本和 AI Agent

```powershell
.\shelllf-cli.exe hosts list --json
'{"host":"主机ID","command":"uname -a"}' | .\shelllf-cli.exe exec --json
```

远程命令较复杂时，用 `exec --stdin 主机ID` 从标准输入读取命令，避免 PowerShell 转义改变管道、引号或 `$`。密码不要写在命令行中。

需要直接输入 `shelllf-cli` 而不是完整路径时，在「设置 → 外部 CLI」安装命令行工具，然后重新打开 PowerShell。查看具体选项：

```powershell
.\shelllf-cli.exe exec --help
.\shelllf-cli.exe upload --help
.\shelllf-cli.exe download --help
.\shelllf-cli.exe sync --help
```

退出码 `255` 表示 CLI 未完成请求，错误说明会打印出来。遇到未运行或未启用错误，先确认 ShellLF 已打开、外部 CLI 已启用，而且两个程序使用同一个用户和数据目录。
