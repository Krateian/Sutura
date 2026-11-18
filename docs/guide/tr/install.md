# Kurulum ve gereksinimler

[← README'ye dön](../../../README.tr.md) · [English](../install.md)

## Gereksinimler

Sutura yerel bir programdır. Desteklenen hiçbir platformda **Python, sanal
ortam, conda, Homebrew, Rust veya C++ derleyicisi gerekmez**.

Desteklenen platformlar, Python sürümünün kapsadığı platformların tam olarak
aynısıdır; yani geçişte hiçbir şey kaybedilmez:

* **Linux x86_64**
* **Linux aarch64**
* **Apple Silicon üzerinde macOS** (arm64)
* **Intel üzerinde macOS** (x86_64)

Bunların dışındaki makineler için yayımlanmış paket yoktur. Böyle bir makinede
`--check-update` ve `--repair-install` hiçbir şey indiremez ve GUI bunu sessizce
başarısız olmak yerine söyler.

* Linux, GUI için tipik bir masaüstü (X11 veya Wayland ve OpenGL) ve olağan
  `tar` ister. KDE Plasma isteğe bağlıdır: kurulumcu mümkün olduğunda Dolphin
  servis menüsünü ekler, CLI ve GUI onsuz da çalışır.
* macOS 12 veya üstünü ister. Uygulama notarize edilmemiştir, bu yüzden ilk
  açılışta *tanımlanamayan geliştirici* uyarısı görünür — aşağıya bakın.

## Kurulum

### İndirilen paketten

Makineniz için paketi [sürümler
sayfasından](https://github.com/Krateian/Sutura/releases/latest) indirin ve
kurulumcuyu paketin yanına koyun:

| Platform | Paket |
|---|---|
| Linux x86_64 | `Sutura-rust-v<version>-x86_64.tar.gz` |
| Linux aarch64 | `Sutura-rust-v<version>-aarch64.tar.gz` |
| macOS arm64 | `Sutura-rust-v<version>-arm64.dmg` |
| macOS Intel | `Sutura-rust-v<version>-x86_64.dmg` |

Linux:

```sh
bash install-rust.sh --from Sutura-rust-v<version>-x86_64.tar.gz
```

macOS:

```sh
bash install-macos-rust.sh --from Sutura-rust-v<version>-arm64.dmg
```

İki kurulumcu da `uname -m` okur, yani *sizin* mimarinize ait paketi bekler ve
tahmin etmek yerine reddeder. `--from` vermezseniz kurulumcu yanında duran en
yeni paketi kullanır ve doğru mimariyi kendisi seçer. `--dry-run` (veya
`SUTURA_DRY_RUN=1`) neyin yazılacağını yazdırır ve hiçbir şeyi değiştirmez.

> **Sürümler sayfasından indirme henüz etkin değil.** Rust paketleri *Build Rust
> distribution artifacts* iş akışı tarafından derlenir; bu iş akışı onları iş
> akışı çıktısı olarak yükler ve bilerek hiçbir sürüm oluşturmaz. İlk Rust
> sürümü yayımlanana kadar kurulumcular yalnızca sizin sağladığınız bir paketle
> çalışır. Bu bölüm kurulacak akış için yazılmıştır ve buradaki hiçbir adım
> henüz var olmayan bir bağlantıya dayanmaz.

### Bütünlük kontrolü

Her paket yanında bir `<package>.sha256` taşır ve güncelleyici bunu **zorunlu**
tutar: özeti eşleşmeyen bir güncelleme, diskte hiçbir şeye dokunulmadan
reddedilir (`sha256 mismatch (expected …, got …); not installed`).

Kabuk kurulumcuları daha esnektir; bunu bilmekte fayda var:

| | `.sha256` varsa | Yoksa |
|---|---|---|
| `install-rust.sh`, `install-macos-rust.sh` | doğrulanır, uyuşmazlık kurulumu **durdurur** | `note: no …sha256 beside the package; skipping integrity check` yazar ve **devam eder** |
| Güncelleyici ve `--repair-install` | doğrulanır, uyuşmazlık ölümcüldür | hiç ilerleyemez |

Yani bir paketi elle indirdiyseniz `.sha256` dosyasını yanında tutun. İsterseniz
önce kendiniz doğrulayın:

```sh
sha256sum -c Sutura-rust-v<version>-x86_64.tar.gz.sha256   # Linux
shasum -a 256 -c Sutura-rust-v<version>-arm64.dmg.sha256   # macOS
```

### Neler kurulur

Linux — her şey ev dizininizin altında, sistem genelinde hiçbir şey:

| Yol | Ne |
|---|---|
| `~/.local/share/sutura/bin/sutura` | CLI |
| `~/.local/share/sutura/bin/sutura-gui` | GUI |
| `~/.local/share/sutura/bin/ftetwild` | paketlendiğinde fTetWild ikilisi |
| `~/.local/bin/sutura`, `~/.local/bin/sutura-gui` | `PATH` üzerindeki sembolik bağlar |
| `~/.local/share/applications/sutura-app.desktop` | masaüstü girdisi |
| `~/.local/share/kio/servicemenus/sutura.desktop` | Dolphin sağ tık girdisi |
| `~/.local/share/icons/hicolor/*/apps/sutura.png` | simgeler |

macOS:

| Yol | Ne |
|---|---|
| `~/Applications/Sutura.app` | uygulama paketinin tamamı |
| `~/.local/bin/sutura`, `~/.local/bin/sutura-gui` | pakete sembolik bağlar |
| `~/Library/Services/Sutura Quick Action.workflow` | Finder Hızlı Eylemi |
| `~/.local/share/sutura/macos-quick-action.sh` | Hızlı Eylem'in yardımcı betiği |

Kurulumcu `~/.local/bin` dizinini kabuğunuzun okuduğu başlangıç dosyasına
(zsh için `.zshrc`/`.zprofile`, bash için `.bashrc`/`.bash_profile`, yoksa
`.profile`) `# added by Sutura installer` işaretiyle ekler, böylece kaldırma
tam olarak o satırı silebilir.

**Var olan bir `data/` dizini yeniden kurulumda korunur**: bir kenara alınır,
program dosyaları değiştirilir ve veri geri konur.

**Lisans kabul istemi yoktur.** `LICENSE` ve `ATTRIBUTION.md` paketin içinde
gelir ve PolyForm Noncommercial koşulları eskisi gibi geçerlidir; ticari lisans
için yine ayrı bir anlaşma gerekir.

macOS uygulaması ad-hoc imzalıdır, notarize edilmemiştir, bu yüzden ilk açılışta
*tanımlanamayan geliştirici* uyarısı görünür. Bir kez **sağ tık → Aç** ile
açın, veya:

```sh
xattr -dr com.apple.quarantine ~/Applications/Sutura.app
```

### Boyut

Son dört hedefli yeşil derleme olan
[44181354813](https://github.com/Krateian/Sutura/actions/runs/44181354813)
(`6ddf165`) koşusunun paketlerine karşı ölçüldü. İndirme boyutu artifact'ın
kendisi; kurulu boyut açılmış paketin `du -sk` değeri (1 MiB = 1024 KiB).
Python sütunu karşılaştırma için v0.9.9'un yayımlanmış sürümüdür.

| | Linux x86_64 | Linux aarch64 | macOS arm64 | macOS Intel |
|---|---|---|---|---|
| **Rust** indirme | 21.8 MiB | 18.9 MiB | 18.4 MiB | 22.3 MiB |
| **Rust** kurulu | 63.5 MiB | 49.8 MiB | 43.2 MiB | 51.0 MiB |
| **Python 0.9.9** indirme | 195.3 MiB (AppImage) | yayımlanmadı | 203.0 MiB | 203.0 MiB (aynı universal paket) |
| **Python 0.9.9** kurulu | — | — | 459.5 MiB | 459.5 MiB (aynı universal paket) |

Bu tablonun dürüstçe söylediği iki şey:

- **Python sürümünde hiçbir zaman Linux aarch64 paketi yoktu**, yani orada
  ölçülecek bir şey yok. Rust derlemesi bu platforma paket veriyor, dolayısıyla
  o platform bir paket kazandı, kaybetmedi.
- **Python'un kurulu boyutu yalnız macOS'ta ölçüldü.** Linux AppImage'i yalnız
  Linux'ta çalışan bir squashfs; büyük/küçük harf duyarsız bir dosya sisteminde
  açmak `terminfo` ağacının ortasında başarısız oluyor, yani oradaki bir `du`
  savunamayacağım bir sayı olurdu. Tahmin etmek yerine `—` bırakıldı.

Tipik mesh onarım süresi bilerek boştur. En çok önem taşıyan sayı odur ve henüz
kesinleşmemiştir: hâlâ verilen sadakat kararına bağlı, dolayısıyla şimdi
doldurmak sonradan değiştirmek demek olurdu.

## Güncelleme

Güncellemeler **`sutura --update`** ile veya GUI'nin güncelleme panelinden
kurulur.

```sh
sutura --check-update   # sadece bildir: daha yeni bir sürüm var mı?
sutura --update         # sorduktan sonra kur
```

`--check-update` hiçbir şey kurmaz. `update available: <tag>`, `no update
available` veya sürüm bir lisans sınırını geçtiğinde bir lisans bildirimi
yazdırır — bu son durumda bilerek reddeder ve sizi sürümler sayfasına
yönlendirir.

`--update` **hiçbir şey yapmadan önce sorar** ve yanıt varsayılan olarak
**hayırdır**: boş Enter, `n` veya tanınmayan herhangi bir cevap kurulumunuzu
olduğu gibi bırakır. `--yes` sizin adınıza cevap verir. Etkileşimsiz bir
çalıştırma (bir betik, CI) da reddeder ve bunu söyler, çünkü soracak kimse
yoktur.

Bir güncelleme şunları yapar:

1. Paketi indirir ve `sha256` değerini doğrular. Özet eşleşene kadar hiçbir şey
   yazılmaz.
2. Mevcut kurulumu bir yedeğe kopyalar.
3. Yeni sürümü açıp yerine koyar, `data/` dizinine dokunmaz.
4. Yeni ikiliyi `--version` ile çalıştırır ve beklenen sürümle cevap verdiğini
   kontrol eder.
5. Bu kontrol başarısız olursa **önceki sürümü geri koyar** ve nereden geri
   yüklendiğini söyler.

### Yedekler ve geri alma

İki yedek saklanır: `~/.local/share/sutura.backup/<version>/`. `data/` hiçbir
zaman bir yedeğe kopyalanmaz, böylece hiçbir yedek geçmişinizin üzerine geri
yüklenemez.

```sh
sutura --rollback   # sorduktan sonra en yeni yedeği geri yükle
```

`--rollback` yalnızca **en yeni** yedeği geri yükler. Daha eskisini seçecek bir
komut yoktur: üçüncü güncelleme olduğunda üçüncü en yeni yedek silinir. Python
dönemine ait bir yedeğin başka yerde tutulmasının nedeni de budur — bkz.
[MIGRATE.md](../../../MIGRATE.tr.md).

## Hasarlı bir kurulumu onarma

```sh
sutura --repair-install   # bu sürümü kendi üzerine yeniden kur
```

**Çalıştırdığınız** sürümün paketini yeniden indirir — asla en yenisini, yani
sessizce güncelleyemez veya sürüm düşüremez — doğrular, bir güncellemeyle aynı
yedekleme ve geri alma yoluyla yerine koyar ve önce sorar (varsayılan: hayır).
GUI'de aynı şey **Kurulumu onar** olarak bulunur.

## Sürüm notları

```sh
sutura --whats-new   # son kurulumun kaydettiği notlar, çevrimdışı
```

Notlar yalnızca yeni bir sürüm başarıyla kurulduktan sonra yazılır, böylece
başarısız bir güncelleme sizi sahip olmadığınız bir sürümün notlarını okurken
bırakamaz. Yeni bir kurulumda henüz kayıtlı bir şey olmayabilir; bu durumda
komut sürümler sayfasını işaret eder.

## Kaldırma

```sh
bash uninstall-rust.sh           # programı kaldır, verini koru
bash uninstall-rust.sh --purge   # veri, ayar ve yedekleri de kaldır
SUTURA_DRY_RUN=1 bash uninstall-rust.sh   # neyin kaldırılacağını göster
```

`--purge` olmadan programı, başlatıcıları, masaüstü girdilerini, simgeleri,
macOS paketini ve Hızlı Eylemi ve kurulumcunun eklediği `PATH` satırını
kaldırır. `~/.local/share/sutura/data` dizinini ve güncelleme yedeklerini
**korur** ve nerede olduklarını söyler.

`--purge` ek olarak `~/.local/share/sutura` (veri dahil), `~/.config/sutura` ve
`~/.local/share/sutura.backup*` dizinlerini kaldırır — hem güncelleme yedeklerini
hem de varsa karantinaya alınmış Python dönemi yedeğini.

## Python sürümünden geçiş

Bir Python kurulumu yeniden kurulmaz, taşınır: geçmiş ve öğrenme verileri
aktarılır, önceki kurulum geri dönebileceğiniz bir yerde saklanır ve yeni sürüm
sağlık kontrolünü geçmeden hiçbir şey silinmez. Bkz.
[Python sürümünden geçiş](../../../MIGRATE.tr.md).
