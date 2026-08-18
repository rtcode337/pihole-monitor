//! Pi-hole のクエリを定期的に取り込んで SQLite に貯める。
//!
//! **「ブロックされていない怪しい通信」を見るための土台。** ブロック済みの一覧
//! (`domain_notes`)は Pi-hole をその場で叩いて集計すれば済むが、こちらは
//! 「いつもと違うか」を言うのが目的なので、**比較対象になる過去が要る**。
//! Pi-hole 自身も長期DBを持っているものの、画面を開くたびに数万件をHTTPで
//! 引き直すのは重いので、手元に写しを持つ。
//!
//! 貯め方は2つに分かれている:
//!
//! | | 何を | どこまで残すか | 何に使うか |
//! |---|---|---|---|
//! | 定期取り込み | 生のクエリ1件ずつ | 保持期間(既定7日) | 周期・種別・クライアント |
//! | 遡り取り込み | 日ごとのドメイン集計 | ずっと(`dns_domains`) | 初出(はじめて見た日) |
//!
//! **遡りに生のクエリを使わない**のが要点。30日ぶんは実測で136万件あり、
//! 1リクエスト1万件の上限では136回のページ送りになる。集計の口
//! (`/api/stats/database/top_domains`)なら1日ぶんが1リクエスト・約60KBで、
//! しかも1回しか出ていないドメインも省略されずに入る(初出はまさにそこを見る)。

use anyhow::Result;

use crate::config::Config;
use crate::db::Db;
use crate::pihole::PiholeClient;

/// 取り込みの窓を重ねる幅(秒)。**境界ぴったりで切らない** ——
/// Pi-hole 側の記録が時刻順に確定するとは限らず、重ねないと取りこぼす。
/// 重複は `dns_queries.id` が弾く。
const OVERLAP_SECS: f64 = 60.0;

/// 初回(カーソルが無いとき)にどこまで生のクエリを遡るか。
/// 保持期間ぶんまで取ってもよいが、初回の1回で数十万件を入れると重いので短くしてある
/// ——足りないぶんは周回を重ねるうちに埋まる。
const FIRST_RUN_LOOKBACK_SECS: f64 = 6.0 * 3600.0;

/// 定期取り込みを回し続ける。**1回の失敗で止めない** ——
/// Pi-hole の再起動やネットワークの瞬断で監視ごと死ぬのを避ける。
pub async fn run(db: Db, pihole: PiholeClient, config: Config) {
    if !config.dns_ingest_enabled {
        tracing::info!("DNSの取り込みは無効(DNS_INGEST_ENABLED=false)");
        return;
    }

    tracing::info!(
        interval_secs = config.dns_ingest_interval.as_secs(),
        retention_days = config.dns_retention_days,
        backfill_days = config.dns_backfill_days,
        "DNSの取り込みを開始する"
    );

    let mut ticker = tokio::time::interval(config.dns_ingest_interval);
    // 取りこぼした周回をまとめて取り返さない(相手を続けて叩くだけで、得るものが同じため)
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        ticker.tick().await;

        // **遡りは毎周回ためす。** 起動時に1回だけだと、そのとき Pi-hole が落ちていたり
        // 認証が通らなかったりしたぶんが**再起動するまで永久に埋まらない**
        // (実際にセッション枠を使い切って踏んだ)。済んでいれば設定を1回読むだけで抜ける
        if let Err(e) = backfill(&db, &pihole, config.dns_backfill_days).await {
            tracing::warn!(error = ?e, "遡り取り込みに失敗した(次の周回でやり直す)");
        }

        match ingest_once(&db, &pihole, &config).await {
            Ok(0) => tracing::debug!("新しいクエリは無かった"),
            Ok(n) => tracing::info!(inserted = n, "クエリを取り込んだ"),
            Err(e) => tracing::warn!(error = ?e, "クエリの取り込みに失敗した(次の周回で取り直す)"),
        }
    }
}

/// 1周ぶんの取り込み。戻り値は新しく入った件数。
async fn ingest_once(db: &Db, pihole: &PiholeClient, config: &Config) -> Result<usize> {
    let now = unix_now();
    let cursor = db.ingest_cursor().await?;
    let since = match cursor {
        Some(ts) => ts - OVERLAP_SECS,
        None => now - FIRST_RUN_LOOKBACK_SECS,
    };

    let records = pihole.queries_since(since).await?;
    if records.is_empty() {
        return Ok(0);
    }

    // **Pi-hole の DB が作り直されると id が振り直される。** そのままだと新しい行が
    // 「見たことのある id」として弾かれ続け、静かに取り込みが止まる
    let max_incoming = records.iter().map(|r| r.id).max().unwrap_or(0);
    let max_known = db.max_query_id().await?;
    if max_known > 0 && max_incoming < max_known {
        tracing::warn!(
            max_known,
            max_incoming,
            "Pi-hole の id が巻き戻っている(向こうのDBが作り直された)。手元の生クエリを捨てて取り直す"
        );
        db.reset_queries().await?;
    }

    let newest = records.iter().map(|r| r.ts).fold(f64::MIN, f64::max);
    let inserted = db.insert_queries(records).await?;

    // **カーソルは実際に取れたところまでしか進めない**(now まで進めると、
    // 取りこぼしたぶんを二度と取りに行かなくなる)
    if newest > f64::MIN {
        db.set_ingest_cursor(newest).await?;
    }

    let before = now - config.dns_retention_days as f64 * 86_400.0;
    let pruned = db.prune_queries(before).await?;
    if pruned > 0 {
        tracing::debug!(pruned, "保持期間を過ぎた生のクエリを消した");
    }

    Ok(inserted)
}

/// 遡り取り込み。1日ずつ集計を引いて `dns_domains` の初出を埋める。
///
/// **すでに終えた日数は覚えておき、設定を伸ばしたぶんだけ足す**(毎回30回叩かない)。
async fn backfill(db: &Db, pihole: &PiholeClient, want_days: i64) -> Result<()> {
    let done = db.backfilled_days().await?;
    if done >= want_days {
        tracing::debug!(done, want_days, "遡り取り込みは足りている");
        return Ok(());
    }

    let now = unix_now() as i64;
    let today_start = jst_day_start(now);
    tracing::info!(from_day = done + 1, to_day = want_days, "遡り取り込みを始める");

    // 古い日から新しい日へ進める。**途中で落ちてもそこまでは記録する**ので、
    // 次の起動が続きから取る
    for day in (done + 1..=want_days).rev() {
        let start = today_start - day * 86_400;
        let end = start + 86_400;
        match pihole.domain_counts(start, end).await {
            Ok(counts) if counts.is_empty() => {
                tracing::debug!(day, "その日の記録は無かった");
            }
            Ok(counts) => {
                let n = counts.len();
                db.merge_domain_counts(counts, start).await?;
                tracing::debug!(day, domains = n, "遡り取り込み");
            }
            Err(e) => {
                tracing::warn!(day, error = ?e, "遡り取り込みを打ち切る(次回続きから)");
                return Ok(());
            }
        }
    }

    db.set_backfilled_days(want_days).await?;
    let stats = db.ingest_stats().await?;
    tracing::info!(
        domains = stats.domains,
        days = want_days,
        "遡り取り込みが終わった"
    );
    Ok(())
}

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// その時刻が属する「日本時間の日」の 00:00 を unix秒で返す。
/// **日付の境界は日本時間で数える**(UTCだと日本の朝9時までが前日に入る)。
fn jst_day_start(ts: i64) -> i64 {
    const JST_OFFSET_SECS: i64 = 9 * 3600;
    (ts + JST_OFFSET_SECS).div_euclid(86_400) * 86_400 - JST_OFFSET_SECS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jst_day_start_uses_japan_boundary() {
        // 2026-08-18 08:00 JST = 2026-08-17 23:00 UTC。**日本時間ではまだ18日**なので、
        // その日の始まりは 2026-08-18 00:00 JST = 2026-08-17 15:00 UTC
        let t = 1_787_007_600; // 2026-08-18T08:00:00+09:00
        let start = jst_day_start(t);
        assert_eq!(t - start, 8 * 3600, "日本時間の0時からの経過が合わない");
        assert_eq!(jst_day_start(start), start, "境界そのものは動かない");
        assert_eq!(jst_day_start(start + 86_399), start, "同じ日のうちは同じ値");
        assert_eq!(jst_day_start(start + 86_400), start + 86_400, "翌日は1日ぶん進む");
    }
}
