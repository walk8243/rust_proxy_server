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

## キャッシュ機能 (Caching Features)

このプロキシサーバは2種類のキャッシュ戦略を提供しています。URLのプレフィックスによって使い分けることができます。

### 1. 標準キャッシュ (`/cache`)

RFC 7234 に準拠した標準的なキャッシュ動作を行います。`Cache-Control` ヘッダの `max-age` を尊重し、有効期限内のコンテンツをキャッシュから返します。期限切れ（Stale）のコンテンツは返さず、必ずオリジンサーバへ問い合わせを行います。

**使い方:**
パスの先頭に `/cache` を付与します。

**例:**
`http://localhost:3000/cache/api/data` へのリクエストは、ターゲットの `/api/data` へ転送され、レスポンスがキャッシュされます。

### 2. SWRキャッシュ (`/swr`)

Stale-While-Revalidate (SWR) 戦略をサポートします。コンテンツが有効期限切れ（Stale）であっても、猶予期間内であれば即座にキャッシュ（Staleな状態）を返し、バックグラウンドで非同期に新しいコンテンツを取得（Revalidate）してキャッシュを更新します。

これにより、ユーザーへのレスポンス速度を保ちながら、キャッシュの鮮度を維持することができます。

**動作フロー:**

1. **Fresh**: キャッシュが有効期限内 (`age < max-age`)  
   👉 **キャッシュを返す**
2. **Stale**: 有効期限切れだが猶予期間内 (`max-age < age < max-age + swr`)  
   👉 **Staleキャッシュを即座に返し、バックグラウンドで更新**
3. **Expired**: 完全に期限切れ  
   👉 **オリジンサーバへ同期的に問い合わせ**

**使い方:**
パスの先頭に `/swr` を付与します。

**例:**
`http://localhost:3000/swr/api/feed` へのリクエストは、ターゲットの `/api/feed` へ転送されます。オリジンサーバが `Cache-Control: max-age=60, stale-while-revalidate=300` のようなヘッダを返している場合に有効です。

