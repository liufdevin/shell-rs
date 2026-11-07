# 发布

[返回 README](../README.zh-CN.md)

在 `CHANGELOG.md`（英文）和 `CHANGELOG.zh.md`（中文）里各写好 `## [x.y.z]` 一节，把 `Cargo.toml` 的版本改成 `x.y.z`，提交后推送 tag `vx.y.z`。`.github/workflows/release.yml` 依次：

1. 核对 tag 与 `Cargo.toml` 一致，两份更新日志都有这一节，从英文版截出 GitHub Release 的说明，确认代码里有发布公钥；
2. 构建并打包：macOS 按 `aarch64`（Apple Silicon）和 `x86_64`（Intel）分别构建、签名、公证、staple，各自产出升级用的 `.app.zip` 和首次安装用的 `.dmg`；Windows 用 Inno Setup 打安装程序，另打便携版的 zip（根上是 `shellrs.exe`、`shelllf-cli.exe` 和空的 `data` 文件夹，用 7-Zip 打包，打完核对正好是这三项），它既是首次下载的包、也是便携版升级用的包；Linux 打 AppImage；
3. 在 `release` 环境里人工批准后：给每个包签名，上传到 R2（`dl.shellrs.com/releases/<版本>/`），建 GitHub Release，**最后**才替换更新清单 `dl.shellrs.com/update/v1/<通道>.json` 并清 CDN 缓存，然后调用官网的部署挂钩重新构建 shellrs.com（官网的下载页和更新日志在构建时读这份清单）。清单里每个包有两个下载地址：先 R2，下载失败时客户端接着试 GitHub Release（仓库是公开的，下载不用登录）。

版本号带 `-` 的（如 `0.3.0-beta.1`）只发到 beta 通道；正式版同时成为 beta 通道的最新版。在 Actions 里手动运行这个 workflow 只构建打包、不发布，用来演练。客户端只接受比自己新的版本，所以发错的版本撤不回来：把清单改回上一版能阻止更多人升级，修复要发新的补丁版。

一次性准备：

- **更新签名密钥**：`minisign -G -p shellrs-update.pub -s shellrs-update.key` 生成两对（一对日常用，一对离线保存备用），把两个公钥（`.pub` 文件的第二行）填进 `src/update/build_info.rs` 的 `TRUSTED_KEYS`。私钥和口令只放进 GitHub 的 `release` 环境，另外离线备份；丢了私钥，已发出去的 ShellRS 就再也收不到更新。换钥匙时先发一版带上新公钥，再换签名用的私钥。
- **Cloudflare**：shellrs.com 的 DNS 托管在 Cloudflare；建 R2 bucket，绑定自定义域 `dl.shellrs.com`，关闭 `r2.dev` 地址；Cache Rules 让 `/releases/*` 和 `/update/*` 按源站的 Cache-Control 缓存（默认不缓存 `.json`、`.AppImage`）；一个只能写这个 bucket 的 R2 API Token，一个只能清这个 zone 缓存的 API Token；官网项目（独立的私有仓库，由 Cloudflare 连着仓库构建）在「设置 › 构建 › 部署挂钩」里建一个构建 `main` 分支的挂钩。
- **Apple**：Developer ID Application 证书（导出 .p12）和 notarytool 用的 App Store Connect API Key。`packaging/macos/Info.plist.in` 里的 bundle id `com.shellrs.ShellRS` 和签名的 Team ID 一经发布就不能再改：钥匙串按它们授权，改了之后每条保存的密码都要重新允许。
- **图标**：`assets/logo/shelllf.png` 是用户提供的源图；运行 `python scripts/build-icons.py` 生成 `shelllf.ico`（Windows 程序与安装程序）和 `shelllf.icns`（macOS）。Linux 和 macOS 开发运行使用源 PNG。软件显示名称为 ShellLF，现有数据目录、钥匙串标识和 CLI 命令保持兼容。
- **GitHub secrets**：仓库级的 `APPLE_CERTIFICATE_P12`（base64）、`APPLE_CERTIFICATE_PASSWORD`、`APPLE_SIGNING_IDENTITY`、`APPLE_TEAM_ID`、`APPLE_API_KEY_P8`（base64）、`APPLE_API_KEY_ID`、`APPLE_API_ISSUER`；`release` 环境（设为需要审批）里的 `MINISIGN_SECRET_KEY`、`MINISIGN_PASSWORD`、`R2_ACCOUNT_ID`、`R2_ACCESS_KEY_ID`、`R2_SECRET_ACCESS_KEY`、`R2_BUCKET`、`CF_ZONE_ID`、`CF_API_TOKEN`、`SITE_DEPLOY_HOOK`（官网部署挂钩的地址，拿到它的人能触发官网重新构建，泄露了就删掉重建）。

清单格式见 `packaging/manifest.example.json`，由 `packaging/make-manifest.sh` 生成：一个文件里是清单原文和它的 minisign 签名，签名的 trusted comment 写明通道和版本。格式只增加字段、不改已有的；已发出去的 ShellRS 一直按 `/update/v1/` 这个地址检查。

macOS 产物名分别为 `ShellRS-<版本>-macos-aarch64.{app.zip,dmg}` 和 `ShellRS-<版本>-macos-x86_64.{app.zip,dmg}`。更新清单保留原有的两个平台键，分别记录对应架构包的地址、大小和 SHA-256；已有 universal 客户端按当前运行的架构升级。Apple Silicon 上通过 Rosetta 运行 Intel 版时仍会收到 Intel 包，需要手动安装 ARM 版才能切换。官网在独立仓库中，下载页也需提供 Apple Silicon / Intel 两个入口，读取清单中各自的 installer 地址。

Windows 便携版的产物名为 `ShellRS-<版本>-windows-x86_64-portable.zip`，清单里是新的平台键 `windows-x86_64-portable`（`assets` 和 `installers` 都指向这个 zip；旧客户端不认这个键，照旧用 `windows-x86_64` 的安装程序）。这个键一经发布就不能再改。官网下载页在清单里有这个键时才显示 Windows 的第二个按钮「便携版」。
