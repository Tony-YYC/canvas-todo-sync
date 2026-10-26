# 获取 Microsoft To Do 凭据

程序通过 Microsoft Graph API 读写 To Do，需要一个 **refresh token**。登录一次后，
程序会把它保存在 `~/.local/state/canvas-todo-sync/ms_token.json`（权限 0600），
之后每次运行都会自动刷新并保存新的 refresh token，不需要再手动操作。

> refresh token 连续 90 天未使用会失效。定时任务每小时运行一次，正常情况下不会过期。
> 如果你修改了 Microsoft 账户密码，或在 <https://account.live.com/consent/Manage> 中撤销了授权，
> 需要重新登录。

有两种登录方式，**推荐先用方式一**，不需要注册任何东西。

---

## 方式一：浏览器授权 + 粘贴回调地址（默认）

适用于原版 TodoSync 注册的公共应用（client_id `49694ef2-8751-4ac9-8431-8817c27350b4`，名称 MyTodoApp），
也适用于你自己注册的应用。服务器无需图形界面，浏览器可以在你的电脑或手机上打开。

```sh
canvas-todo-sync auth login
```

1. 程序会输出一个 `https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize?...` 链接，
   复制到任意设备的浏览器中打开；
2. 登录 Microsoft 账户，核对并同意授权。请求的权限只有：
   - 读取和写入你的任务（`Tasks.ReadWrite`）
   - 登录并读取你的个人资料（`User.Read`）
   - 保持对已授予访问权限的数据的访问（`offline_access`，用于获取 refresh token）
3. 同意后浏览器会跳转到一个**空白页面**，地址形如
   `https://login.microsoftonline.com/common/oauth2/nativeclient?code=M.C5...&state=...`；
4. 复制这个空白页面的**完整地址**，粘贴回服务器终端并回车。

看到 `登录成功：<你的名字>` 即完成。授权码只在几分钟内有效，过期了就重新运行一次。

本程序使用了 PKCE 和 state 校验，即使回调地址被别人看到，也无法用它换取令牌。

## 方式二：设备代码（需要自己注册应用）

设备代码流更省事：服务器上显示一个短代码，你在任意设备打开
<https://microsoft.com/devicelogin> 输入即可，不需要复制长地址。
但原版的公共应用**没有开启**“允许公共客户端流”（已实测：`AADSTS70002`），
所以需要使用你自己注册的应用。自己注册应用的另一个好处是：不依赖原作者的应用一直存在。

### 注册应用

1. 用你的 Microsoft 账户登录 <https://entra.microsoft.com>（或 <https://portal.azure.com> →
   “Microsoft Entra ID”），进入 **应用注册 → 新注册**；
   - 如果提示个人账户没有目录/租户，需要先免费开通 Azure 账户，
     或者直接使用方式一，不必注册应用。
2. 填写：
   - 名称：随意，例如 `canvas-todo-sync`
   - 受支持的帐户类型：**仅个人 Microsoft 帐户**（如果要用学校/工作账户，选择“任何组织目录中的帐户和个人 Microsoft 帐户”）
   - 重定向 URI：平台选 **公共客户端/本机(移动和桌面)**，
     值填 `https://login.microsoftonline.com/common/oauth2/nativeclient`
     （填了这个之后方式一也能用）
3. 注册完成后，在“概述”页复制 **应用程序(客户端) ID**；
4. 进入 **身份验证**，在“高级设置”中把 **允许公共客户端流** 设为 **是**，保存；
5. （可选）**API 权限 → 添加权限 → Microsoft Graph → 委托的权限**，勾选
   `Tasks.ReadWrite`、`User.Read`。不加也可以，登录时会请求用户同意。

不需要创建客户端密码（client secret），这是公共客户端。

### 使用

在配置文件中填入 client_id：

```toml
[microsoft]
client_id = "你的应用程序(客户端) ID"
tenant = "consumers"   # 个人账户；学校/工作账户改为 organizations
```

然后：

```sh
canvas-todo-sync auth login --device
```

按提示在浏览器中输入代码并授权即可。

---

## 管理

```sh
canvas-todo-sync auth status   # 查看登录状态（会实际刷新一次令牌并读取用户信息）
canvas-todo-sync auth logout   # 删除本地保存的令牌
canvas-todo-sync check         # 同时检查配置、Canvas 令牌和 Microsoft 令牌
```

令牌文件里记录了获取它时使用的 client_id 与 tenant；之后修改配置中的 client_id 不会影响已保存的令牌，
需要重新 `auth login` 才会切换。

## 常见错误

| 错误 | 原因与解决 |
| --- | --- |
| `invalid_grant` | refresh token 失效（长期未用、改了密码、撤销了授权），重新 `auth login` |
| `AADSTS70002` / `AADSTS7000218` | 该应用未开启“允许公共客户端流”，不能用 `--device` |
| `AADSTS50011` redirect_uri 不匹配 | 自己注册的应用没有添加 `nativeclient` 重定向 URI |
| `AADSTS700016` 找不到应用 | client_id 填错，或 tenant 与应用支持的账户类型不匹配 |
| `AADSTS9002313` / `invalid_request` | 粘贴的地址不完整，重新运行 `auth login` |
