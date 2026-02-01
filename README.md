# Rust Proxy Server

Rustで作られたシンプルなリバースプロキシサーバです。
指定された宛先（ターゲットURL）へリクエストをフォワードします。

## 必要要件

- Rust (cargo) がインストールされていること

## 実行方法

`cargo run` コマンドで実行できます。`--target` オプションでフォワード先のURLを指定してください。

```bash
cargo run -- --target <フォワード先のURL>
```

### オプション

- `-t`, `--target <URL>`: フォワード先のURL (必須)
- `-p`, `--port <PORT>`: リッスンするポート番号 (デフォルト: 3000)

### 実行例

httpbin.org を宛先として起動する場合:

```bash
cargo run -- --target https://httpbin.org
```

ポート 8080 で起動する場合:

```bash
cargo run -- --target https://httpbin.org --port 8080
```

## 動作確認方法

サーバを起動した状態で、ブラウザや `curl` コマンドを使って `localhost` にアクセスします。

例: `https://httpbin.org` をターゲットに起動している場合

```bash
# クエリパラメータ付きのリクエスト
curl "http://localhost:3000/get?foo=bar"
```

これにより、`https://httpbin.org/get?foo=bar` からのレスポンスが返ってくれば成功です。
