# Python sürümünden geçiş

[English](MIGRATE.md) · [Türkçe](MIGRATE.tr.md)

Sutura eskiden bir Python uygulamasıydı. Artık Python'a, sanal ortama veya
conda'ya ihtiyaç duymayan yerel bir program. Bu sayfa mevcut bir Python
kurulumuna ne olduğunu, nelerin korunduğunu ve bir sorun çıkarsa nasıl geri
dönüleceğini anlatır.

Çoğu kişinin hiçbir şey yapmasına gerek yok: son Python sürümü yeni sürüme kendi
kendine devredebilir. Bu sayfanın geri kalanı, geçişi elle çalıştırmak
istediğinizde veya tam olarak neyin değiştiğini bilmek istediğinizde içindir.

## Kısa özet

* **Geçmişiniz ve öğrenme verileriniz korunur**.
* Bir şey değiştirilmeden önce **önceki kurulum kaydedilir**.
* Yeni sürüm başlamazsa **Python kurulumu geri getirilir**.
* Yeni sürümün çalıştığı kanıtlanmadan hiçbir şey silinmez.

## Neler korunur

| Ne | Nereye gider |
|---|---|
| Onarım geçmişi | `~/.local/share/sutura/data/history.jsonl` |
| Sınıflandırma öğrenme sayaçları | `~/.local/share/sutura/data/triage_learning.json` |
| Ayarlarınız | `~/.config/sutura` içinde değişmeden kalır |
| `data/` içindeki diğer her şey | olduğu yerde kalır |

`data/` dizini hiçbir zaman bir yedeğe kopyalanmaz, değiştirme sırasında
silinmez ve üzerine geri yüklenmez. Bu kural güncellemelerin kullandığı kuralın
aynısıdır: kullanıcı verisi size aittir, kurulumcunun değil.

## Eski sürüm nereye gider

**Okumaya değer kısım burası.** Herhangi bir şey değiştirilmeden önce önceki
kurulum bir yedek olarak saklanır. Python dönemine ait bir yedeğin yapısı
yenilerden farklıdır, bu yüzden onlarla karıştırılmak yerine *karantinaya*
alınır:

```
~/.local/share/sutura.backup.python-<zaman>
```

Zaman damgası geçişin tarih ve saatidir. Bilerek döngüsel `sutura.backup/`
dizininin **dışında** tutulur, çünkü o dizin yalnızca en yeni iki sürümü saklar
ve eskilerini sessizce siler. O döngüye yakalanan bir Python yedeği, iki kez
güncelleme yaptığınız ilk anda yok olurdu — ve geri dönmeye karar verirseniz hâlâ
var olmasını istediğiniz şey tam olarak odur.

Yani bir geçişten sonra şunlara sahip olursunuz:

```
~/.local/share/sutura/                        yeni kurulum
~/.local/share/sutura/data/                   verileriniz, dokunulmamış
~/.local/share/sutura.backup.python-<zaman>/  tüm Python kurulumu, saklanır
```

Daha sonraki bir `--purge` kaldırma işlemi bu Python yedeğini de siler. O zamana
kadar kalır.

## Geçişi kendiniz çalıştırmak

Normalde köprü sürümünün güncelleyicisi bunu sizin için yapar. Elle çalıştırmak
için:

```sh
sutura --migrate-from-python --from-artifact /yol/Sutura-rust-v<ver>-<arch>.<ext> --yes
```

İki şey gereklidir ve ikisi de şaşırtıcı bir şey yapmak yerine açıklamayla
reddeder:

* **`--from-artifact`** — indirilen sürüm paketi. Geçiş sizin için bir tane
  tahmin etmez veya indirmez, çünkü doğrulanmış belirli bir ağaca ihtiyaç duyar.
* **`--yes`** — bu, eski çalışma ortamını siler. Bu olmadan komut durur ve
  nedenini söyler. Soru sormaz, bu yüzden sorusu olmayan bir betikten
  çalıştırmak güvenlidir.

Taşınacak bir Python kurulumu yoksa komut `Nothing to migrate` bildirir ve
başarıyla çıkar. Bu bir hata değildir.

## Gerçekte olanlar, sırasıyla

1. **Karantina.** Python dönemine ait yedek `sutura.backup.python-<zaman>/`
   konumuna taşınır.
2. **Veri.** Geçmiş ve öğrenme dosyaları yeni `data/` dizinine taşınır.
3. **Yedek ve değiştirme.** Mevcut ağaç yedeklenir, ardından yeni sürüm açılıp
   yerine konur. `data/` baştan sona korunur.
4. **Sağlık kontrolü.** Yeni ikili `--version` ile çalıştırılır. Çıktı beklenen
   sürümle eşleşmelidir.
5. **Silme, ancak şimdi.** Sanal ortam (`venv`, `venv311`), varsa gömülü Python
   ve Python kaynakları silinir. Silmeyi kalıcı yapan budur — kontrolten sonra
   olur, asla önce değil.

Adım 4 başarısız olursa Python kurulumu geri gelir ve komut nereden geri
yüklendiğini söyler. Hiçbir şey kaybolmadı.

## Lisanslar

Geçiş sizden yeni bir lisans kabul etmenizi istemez, çünkü bu bir lisans
değişikliği değildir — zaten sahip olduğunuz aynı PolyForm Noncommercial
koşullarını kurar. Ticari kullanım için ayrı bir anlaşma yine gereklidir, eskisi
gibi.

## Geri dönmek isterseniz

Başka bir şey yapmayın ve karantinaya alınmış dizini kullanın: Python
kurulumunun tam bir kopyasıdır. `data/` koruyarak `~/.local/share/sutura`
üzerine geri yükleyin veya bulunduğu yerden çalıştırın.

Yeni sürümün kendi `--rollback` bayrağı en yeni *güncelleme* yedeğini geri
yükler, Python yedeğini değil — Python yedeğinin ayrı tutulmasının ve
döndürülmemesinin nedeni budur.

## Geçişten sonra

* Sürümü kontrol edin: `sutura --version`.
* Geçtiğiniz sürümün notlarını okuyun: `sutura --whats-new`.
* Daha sonra yeni sürümleri kontrol edin: `sutura --check-update` (yalnızca
  bildirir; hiçbir şey kurmaz).

Bundan sonraki güncellemeler `sutura --update` ile kurulur; bu komut önce sorar
ve siz aksini söylemedikçe "hayır" yanıtını verir. Bkz.
[Kurulum ve gereksinimler](docs/guide/tr/install.md).

## Eski kayıtlı sürüm notları hakkında bir not

Sürüm notları `~/.local/share/sutura/data/whatsnew/` altında saklanır ve geçiş
bunu `data/`nın geri kalanıyla birlikte taşır. Python kurulumunuz notları
kaydetmişse, `sutura --whats-new` ilk güncelleme onların yerini alana kadar
onları gösterebilir. Bu, silinmek yerine bilerek olduğu gibi bırakılır.
