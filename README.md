# racer — đua lệnh đầu ngày

Gửi lệnh khách đặt trước giờ lên sở (HOSE, HNX, UPCoM) qua FIX 4.4 ngay khi sở phát **K04 mở phiên**. Mọi việc tốn thời gian (đọc file, kiểm tra luật sàn, xếp thứ tự, mã hoá FIX, logon) làm xong trước giờ mở; lúc K04 tới chỉ còn điền seq/giờ và `write()`.

Viết bằng Rust, chỉ dùng thư viện chuẩn. Message theo đúng cách LT-OMS gửi (tag KRX `128/129`, `544`, `581`, `20000`, `20054`…), nên chạy được với [hnxsim](../exchange_sim) mà không cần sửa sim.

## Chạy

```powershell
cargo build --release
copy .env.example .env                       # sửa giờ, session, rate
.\target\release\racer.exe --dry-run         # kiểm tra lệnh, in kế hoạch, không kết nối
.\target\release\racer.exe                   # chạy thật
.\target\release\racer.exe --env D:\cfg\prod.env
```

Exit code: `0` mọi lệnh được sở nhận, `1` có lệnh bị loại, bị từ chối, không phản hồi hoặc chưa gửi, `2` lỗi cấu hình hoặc khởi động.

## Thử với hnxsim

```powershell
.\scripts\sim-race.ps1                       # 2000 lệnh, sim mở phiên sau 45s
.\scripts\sim-race.ps1 -Count 10000 -Rate 3000
.\scripts\sim-race.ps1 -ClockEarlyMs 300     # đối chứng: bỏ K04, bắn theo đồng hồ sớm 300ms
```

Script sinh config sim (bật `venue` và lịch phiên với K04 AA1/BB1/AB1 vào giờ đã tính), sinh file lệnh, chạy `hnxsim.exe`, chạy racer rồi in số đo của cả hai phía. Sim bật venue nên lệnh vào trước K04 bị từ chối `103=0101`: kết quả có 0101 tức là racer bắn sớm.

Số đo trên máy dev (loopback, 2000 lệnh, 2000 lệnh/s mỗi session, burst 20):

| | K04 → lệnh đầu rời socket | Kết quả |
|---|---|---|
| Bắn theo K04 | 36–56 µs | 2000/2000 được nhận |
| Đối chứng: đồng hồ sớm 300ms | — | 1240 lệnh bị `0101` (đúng số lệnh gửi trong 300ms đầu) |

Script dùng cổng của `acceptor.cfg` cộng `-PortOffset` (mặc định 1000) để không đụng hnxsim đang chạy cho việc khác.

## Đo độ trễ K04 → lệnh

```powershell
cargo run --release --example latency -- --orders 1
cargo run --release --example latency -- --orders 20 --batch 32
cargo run --release --example latency -- --orders 20 --batch 1 --no-busy-poll   # cách cũ
```

Sở giả trên loopback ghi K04 vào socket rồi spin đọc tới khi nhận đủ lệnh D: đo trọn đường K04 → D, kể cả lúc thread đọc của racer tỉnh dậy. Máy dev (i3-12100F, Windows 11, 1900 vòng):

| | p50 | p99 |
|---|---|---|
| 1 lệnh, chặn recv (cũ) | 68.8 µs | 711 µs |
| 1 lệnh, busy-poll | **54.1 µs** | **232 µs** |
| 20 lệnh, mỗi lệnh 1 syscall, chặn recv (cũ): tới lệnh cuối | 188 µs | 903 µs |
| 20 lệnh gộp 1 write, busy-poll: tới lệnh cuối | **48.7 µs** | **176 µs** |

Phần còn lại gần như toàn bộ là syscall: một lần `send()` trên loopback Windows tốn 20–40µs (đo riêng), mỗi lượt có hai lần (K04 của sở giả và lệnh của racer). Code của racer (parse K04, bật cờ, mã hoá lệnh) cỡ 1–2µs. Muốn nhanh hơn nữa phải đổi hạ tầng (Linux, card mạng tốt, kernel bypass), không phải đổi code.

## Luồng chạy

1. Đọc CSV, kiểm tra (lô 100, KL tối đa HOSE, bước giá, LO phải có giá, ATO chỉ ở HOSE). Lệnh sai bị loại và in ra, không chiếm chỗ trong hàng gửi.
2. Xếp thứ tự **lệnh nhận trước đi trước** theo `received_at` (cùng thời điểm thì theo dòng; thiếu `received_at` đứng cuối). Chia lệnh cho session của đúng thị trường, chọn session có hàng chờ dự kiến xả xong sớm nhất. Kế hoạch được ghi và fsync vào `journal/<ngày>_plan.csv` trước khi kết nối, để đối chiếu và giải trình thứ tự gửi.
3. Mã hoá sẵn từng NewOrderSingle.
4. `RACE_CONNECT_LEAD` trước giờ mở: logon mọi session (session lỗi thì thử lại mỗi 2s tới 1s trước giờ mở).
5. Mỗi session lệnh có một thread gửi, chờ cờ của thị trường: ngủ 1ms/lần, từ `RACE_SPIN_LEAD` trước giờ mở thì spin. Cũng từ lúc đó, thread đọc của session bật busy-poll chuyển socket sang non-blocking và spin đọc (không chờ kernel đánh thức), tới khi mọi thị trường đã mở. Thread đọc nhận K04 khớp `RACE_TRIGGER_*` thì bật cờ (một lệnh atomic, việc khác làm sau). Thread gửi thấy ngay, mã hoá các lệnh đang có quota throttle (GCRA: `BURST` lệnh liền rồi đều `RATE` lệnh/giây, tối đa `BATCH` lệnh) vào một buffer và gửi bằng **một** lần `write()`.
6. Chờ phản hồi đầu tiên của từng lệnh (ack `8` 0/0, từ chối `8` 8/8, `3`, `j`) tối đa `RACE_ACK_TIMEOUT`, in tổng kết và logout.

K04 HOSE tới trên session tin (`VOMGW`), HNX trên `VXMGW`, UPCoM trên chính session lệnh `LTHNX`. Racer nghe K04 trên mọi session.

## Cấu hình

Biến môi trường, hoặc file `.env` (biến có sẵn được ưu tiên). Xem [.env.example](.env.example).

| Biến | Mặc định | |
|---|---|---|
| `RACE_OPEN_TIME` | (bắt buộc) | giờ mở dự kiến: `09:00:00[.fff]`, RFC 3339 hoặc `+90s` |
| `RACE_UTC_OFFSET` | `+07:00` | |
| `RACE_CONNECT_LEAD` | `2m` | logon trước giờ mở bao lâu |
| `RACE_SPIN_LEAD` | `3s` | bắt đầu spin trên cờ (mỗi thread gửi chiếm trọn một core) |
| `RACE_K04_EARLIEST` | `10m` | K04 sớm hơn giờ mở quá mức này bị bỏ qua (tin cũ hoặc nhầm) |
| `RACE_FALLBACK_DELAY` | (tắt) | quá giờ mở chừng này mà chưa có K04 thì bắn theo đồng hồ |
| `RACE_GIVE_UP_AFTER` | `5m` | quá giờ mở chừng này mà thị trường chưa mở thì thôi |
| `RACE_ACK_TIMEOUT` | `15s` | chờ phản hồi sau khi gửi xong |
| `RACE_TRIGGER_STO/STX/UPX` | `AA1` / `BB1` / `AB1` | BoardEvtID (tag 20005) mở phiên, nhiều giá trị cách nhau dấu phẩy |
| `RACE_ORDERS_FILE` | `orders.csv` | |
| `RACE_JOURNAL_DIR` | `journal` | kế hoạch, log FIX, file seq |
| `RACE_CLORDID_PREFIX` | `R` | ClOrdID = prefix + YYMMDD + thứ tự 6 số |
| `RACE_SENDER_SUB_ID` | `HSC` | tag 50; để rỗng thì không gửi |
| `RACE_CHECK_TICK` | `true` | tắt nếu đua ETF/CW (bước giá khác cổ phiếu) |
| `RACE_HEARTBEAT` | `30s` | |
| `RACE_BUSY_POLL` | `true` | mặc định busy-poll cho mọi session (mỗi session bật chiếm một core trong cửa sổ đua) |
| `RACE_SESSIONS` | (bắt buộc) | danh sách tên session |
| `RACE_<S>_ADDR/SENDER/TARGET` | (bắt buộc) | |
| `RACE_<S>_MARKETS` | (rỗng) | `STO`, `STX`, `UPX` (hoặc HOSE/HNX/UPCOM); rỗng = chỉ nghe tin |
| `RACE_<S>_RATE` / `_BURST` | `50` / `1` | hạn mức gửi của session |
| `RACE_<S>_BATCH` | `32` | số lệnh tối đa gộp vào một lần `write()` (1 = mỗi lệnh một syscall) |
| `RACE_<S>_BUSY_POLL` | `RACE_BUSY_POLL` | chỉ cần cho session nhận K04; session chỉ nhận ER (vd lệnh HOSE) để `false` cho đỡ tốn core |
| `RACE_<S>_RESET_SEQ` | `false` | `true`: logon với 141=Y từ seq 1; `false`: tiếp seq từ `journal/<ngày>_<S>.seq` |

**Throttle:** khi chưa có số chính thức của sở, cứ để `RATE` thấp. Có số rồi thì đặt thấp hơn hạn mức ~5–10%, vì vượt hạn mức có thể bị sở ngắt session.

## File lệnh

```csv
received_at,account,exchange,symbol,side,ord_type,qty,price
2026-10-24T20:00:00.120+07:00,011C000001,HOSE,VN000000HCM0,B,LO,1000,24200
2026-10-24T20:00:01.731+07:00,011C000003,HOSE,VN000000HCM0,B,ATO,200,
```

`symbol` là ISIN như gửi lên sở. Cột tuỳ chọn: `board_id` (mặc định `G1`), `account_type`, `cash_margin`, `invest_code` (mặc định `1`), `forn_invest_type_code` (mặc định `00`). Không hỗ trợ field có dấu nháy. Xem [orders.sample.csv](orders.sample.csv).

## Cấu trúc

```text
src/main.rs      luồng chạy, định tuyến message (K04 -> trigger, ER -> tracker), tổng kết
src/lib.rs       khai báo module (dùng chung cho binary và examples)
examples/latency.rs  đo độ trễ K04 -> D trên loopback
src/config.rs    đọc env/.env
src/order.rs     CSV, luật sàn trước giờ mở
src/plan.rs      thứ tự FIFO, chia session, ClOrdID
src/fix.rs       encode/decode FIX, OrderTemplate (lệnh mã hoá sẵn)
src/session.rs   FIX session: logon, thread đọc (busy-poll), heartbeat, TestRequest, GapFill, seq, send_batch
src/trigger.rs   cờ kích hoạt theo thị trường (atomic), lọc K04
src/sender.rs    thread gửi: chờ cờ, chọn lệnh theo rank giữa các thị trường, throttle, gộp lô
src/throttle.rs  GCRA
src/track.rs     phản hồi đầu tiên của từng lệnh
src/journal.rs   log FIX trên thread riêng
scripts/sim-race.ps1  chạy thử với hnxsim
```

`cargo test` chạy test đơn vị (FIX, giờ, CSV, luật sàn, chia lệnh, throttle, trigger).

## Chưa làm, cần làm trước khi lên thật

- **Spec FIX của sở:** field và mã đang theo cách LT-OMS gửi cho hnxsim. Phải đối chiếu spec KRX chính thức: độ dài ClOrdID, field bắt buộc, K04 nào thật sự mở nhận lệnh.
- **Kiểm tra rủi ro và phong toả:** racer chưa kiểm sức mua hay chứng khoán; file lệnh đầu vào phải là lệnh đã phong toả ở OMS hoặc core.
- **Trần/sàn:** chưa kiểm vì cần bảng giá trong ngày (K05 hoặc DB).
- **Chiều vào chưa ResendRequest:** gap seq chiều vào chỉ được ghi cảnh báo. Khi sở xin gửi lại, racer trả GapFill chứ không gửi lại lệnh, để tránh lệnh trùng; lệnh thiếu phản hồi sẽ hiện ra lúc tổng kết.
- **Tinh chỉnh máy chạy thật:** colocation gần gateway của sở, ghim CPU cho thread gửi và thread đọc, đồng bộ giờ (PTP/chrony). Đồng hồ chỉ còn dùng cho mốc kết nối, spin và dự phòng. Trong cửa sổ đua, mỗi thread gửi và mỗi session busy-poll chiếm trọn một core: máy phải có đủ core cho số thread đó cộng phần còn lại của hệ thống.
- **Gộp lệnh và throttle của sở:** lô gộp vẫn tôn trọng `RATE`/`BURST`, nhưng nếu sở đếm hạn mức theo từng gói TCP hoặc cấm nhiều message trong một gói thì đặt `BATCH=1`.
- **Bảo mật:** chưa có TLS và chưa có xác thực ngoài CompID.
