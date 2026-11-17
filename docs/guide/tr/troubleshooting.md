# Sorun giderme

[← README'ye dön](../../../README.tr.md) · [English](../troubleshooting.md)


* **macOS uygulamanın tanımlanamayan bir geliştiriciden geldiğini söylüyor.**
  Beklenen durum: paket ad-hoc imzalıdır, notarize edilmemiştir. Bir kez **sağ
  tık → Aç** ile açın veya karantina özniteliğini temizleyin:
  `xattr -dr com.apple.quarantine ~/Applications/Sutura.app`.
* **`--check-update` veya `--repair-install` indirilecek bir şey olmadığını
  söylüyor.** Bu ikisi platformunuz için yayımlanmış bir derleme gerektirir.
  Yalnızca dört tane var: Linux x86_64, Linux aarch64, macOS arm64 ve macOS
  Intel. Bunların dışında tahmin etmek yerine reddeder ve sürümler sayfasını
  işaret eder.
* **Bir güncelleme sonrası program başlamadı.** Önceki sürümü kendisi geri
  koymuş ve nereden geri yüklediğini söylemiş olmalıydı. Geri alma işleminin de
  başarısız olduğunu söylüyorsa önceki sürümünüz hâlâ
  `~/.local/share/sutura.backup/` içindedir; `sutura --rollback` ile geri
  yükleyin veya `data/` dizinini koruyarak `~/.local/share/sutura` üzerine
  kopyalayın.
* **`sutura --rollback` çok geriye gitti veya yeterince geriye gitmedi.**
  Yalnızca **en yeni** yedeği geri yükler ve yalnızca iki yedek saklanır — her
  yeni güncellemede üçüncü en yeni silinir. Daha eskisini seçecek bir komut
  yoktur.
* **Güncelleme sorusu görünmedi.** Yalnızca bir terminal varken sorar. Terminali
  olmayan bir çalıştırma tasarım gereği reddeder ve şunu yazdırır:
  `(no terminal to ask on; pass --yes to confirm)`. Betiklerde `--yes` ekleyin.
* **GUI'de KDE dosya diyaloğu yok.** Yerel diyalog, masaüstünüzde
  `plasma-integration` kurulu olmasını gerektirir. Yoksa araç takımı kendi
  diyaloğuna geri döner — çoklu seçim için Ctrl/Shift+tık yine de çalışır.
* **Sert bir çökme mesaj bırakmadı ama bir çökme kaydı var.** Sutura (CLI veya
  GUI) çöktüğünde Sutura'nın kullanıcı verisi klasörüne bir `crash.log` yazar;
  GUI ayrıca Tanılama'ya işaret eden kısa bir diyalog gösterir. Kayıt yalnızca en
  son bölümünü (~200 KB) tutar. İşletim sistemine göre konum: Linux ve macOS'ta
  `~/.local/share/sutura/data/crash.log`; `SUTURA_DATA_DIR` ortam değişkeni
  ayarlıysa bunun yerine `$SUTURA_DATA_DIR/crash.log`. Windows derlemesi yoktur.
  Dosya kendiliğinden makinenizden çıkmaz — hiçbir şey yüklenmez;
  `sutura --diagnostics out.zip` temizlenmiş bir kopyasını pakete ekler, onu
  kendiniz bir hata bildirimine ekleyebilirsiniz.
* **`--whats-new` kayıtlı not olmadığını söylüyor.** Notlar bir kurulum başarılı
  olduğunda yazılır, bu yüzden yeni bir kurulumda ilk güncelleme veya onarıma
  kadar hiç yoktur. Komut bunun yerine sürümler sayfasını işaret eder.

Bir onarım başarısız olursa veya bir yöntem kullanılamıyor görünürse, üç komut
kurulumu tanılamaya ve kurtarmaya yardımcı olur:

* **`sutura --self-check`** kurulumu, onarım yöntemlerini küçük gömülü bir mesh
  üzerinde çalıştırarak doğrular ve birkaç saniye içinde bir özet satırı
  yazdırır (`Methods: N/19 OK (X failed, Y skipped)`). Yüklenemeyen veya
  başarısız olan bir yöntem kontrolü başarısız kılar; yalnızca kurulu olmayan
  isteğe bağlı bir ek, başarısız değil atlanmış olarak bildirilir. Komut,
  herhangi bir yöntem başarısız olursa sıfırdan farklı bir kodla çıkar. GUI,
  açılışta aynı kullanılabilirlik kontrolünü çalıştırır ve bir yöntem
  kullanılamıyorsa kapatılabilir bir uyarı şeridi gösterir.
* **`sutura --diagnostics out.zip`** sistem bilgilerini, geometri çekirdeğinin
  durumunu, on dokuz yöntemin tümünün kullanılabilirliğini, kullanıcı
  ayarlarını, en son onarım kaydını ve çökme kaydının temizlenmiş bir kopyasını
  (`crash_log.txt`; yalnızca bir çökme kaydı varsa) içeren tek bir ZIP arşivi
  yazar. Kişisel yollar ve kullanıcı adları arşivden temizlenir ve arşive hiçbir
  dosya adı konmaz; böylece bir hata raporuna eklenebilir.
* **`sutura --repair-install`** **çalıştırdığınız** sürümün paketini yeniden
  indirir (asla en yenisini, yani kazara güncelleyemez veya sürüm düşüremez),
  `sha256` değerini doğrular ve yerine koyar. Önce mevcut kurulumu yedekler ve
  yeni ağaç başlamazsa geri alır. Kullanıcı verilerine (geçmiş, öğrenme, ayarlar)
  asla dokunmaz.

  Değiştirmeden önce **sorar ve yanıt varsayılan olarak hayırdır** — Enter'a
  basın, hiçbir şey olmaz. `sutura --repair-install --yes` sizin adınıza cevap
  verir; terminali olmayan bir çalıştırma onay varsaymak yerine reddeder.
  `--self-repair` aynı komutun Python'daki eski adıdır. GUI'deki **Kurulumu
  onar** düğmesi de aynı şeyi yapar.

  Bunun tersini istiyorsanız — bu sürümü yeniden kurmak yerine önceki sürüme
  dönmek — o komut `sutura --rollback`.
