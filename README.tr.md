<p align="center">
  <img src="assets/icon/sutura-128.png" alt="Sutura" width="112">
</p>

<h1 align="center">Sutura</h1>

<p align="center">
  <b>Bozuk STL, OBJ ve 3MF mesh'ler için su geçirmez, basılabilir onarım — Linux ve macOS'ta.</b><br>
  Hasarı onarır, sağlam geometriye dokunmaz ve orijinal dosyayı olduğu gibi bırakır.
</p>

<p align="center">
  <a href="https://github.com/Krateian/Sutura/releases/latest"><img src="https://img.shields.io/github/v/release/Krateian/Sutura?label=release&color=2ea44f" alt="Son sürüm"></a>
  <a href="https://github.com/Krateian/Sutura/releases"><img src="https://img.shields.io/github/downloads/Krateian/Sutura/total?cacheSeconds=3600&color=2ea44f" alt="İndirmeler"></a>
  <img src="https://img.shields.io/badge/platform-Linux%20%7C%20macOS-informational" alt="Platformlar">
  <img src="https://img.shields.io/badge/core-Rust-dea584?logo=rust&logoColor=white" alt="Rust çekirdek">
  <img src="https://img.shields.io/badge/Python-3.11%2B-3776AB?logo=python&logoColor=white" alt="Python 3.11+">
  <br>
  <a href="https://github.com/Krateian/Sutura/actions/workflows/ci.yml"><img src="https://github.com/Krateian/Sutura/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/Krateian/Sutura/actions/workflows/codeql.yml"><img src="https://github.com/Krateian/Sutura/actions/workflows/codeql.yml/badge.svg" alt="CodeQL"></a>
  <a href="https://github.com/Krateian/Sutura/actions/workflows/build-appimage.yml"><img src="https://github.com/Krateian/Sutura/actions/workflows/build-appimage.yml/badge.svg" alt="AppImage derlemesi"></a>
  <a href="https://github.com/Krateian/Sutura/actions/workflows/build-macos.yml"><img src="https://github.com/Krateian/Sutura/actions/workflows/build-macos.yml/badge.svg" alt="macOS derlemesi"></a>
  <br>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-PolyForm%20Noncommercial%201.0.0-blue" alt="Lisans"></a>
  <a href="https://docs.google.com/forms/d/e/1FAIpQLSdqFrO11fjIBACrFwovOKGPpKVdnPcwxW0VRaQfwCqGayfJig/viewform"><img src="https://img.shields.io/badge/commercial%20license-request%20form-blue" alt="Ticari lisans"></a>
  <img src="https://img.shields.io/github/last-commit/Krateian/Sutura" alt="Son commit">
  <img src="https://img.shields.io/github/stars/Krateian/Sutura?style=social" alt="GitHub yıldızları">
</p>

<p align="center">
  <a href="#kurulum">Kurulum</a> ·
  <a href="#kullanım">Kullanım</a> ·
  <a href="#nasıl-çalışır">Nasıl çalışır</a> ·
  <a href="#belgeler">Belgeler</a> ·
  <a href="CHANGELOG.md">Changelog</a> ·
  <a href="README.md">English</a>
</p>

---

Linux'ta Windows'un sağ tık *Fix model* özelliğinin (3D Builder, Netfabb)
doğrudan bir karşılığı yoktur; Bambu Studio'nun *Fix model* düğmesi de Linux'ta
çalışmaz. Sutura bu boşluğu doldurur: mesh'i seçin, onarın ve dilimleyicilerin
kabul ettiği onarılmış bir kopya alın — GUI'den, komut satırından, dosya
yöneticisinde sağ tıkla ya da bir OrcaSlicer eklentisiyle.

<p align="center">
  <img src="assets/screenshot.png" alt="Sutura ana penceresi" width="820">
</p>

## Öne çıkanlar

- **Yalnızca bozuk olanı onarır.** Delikler, yanlış birleşmiş kenarlar, ters
  yüzler, üst üste binen ya da kendini kesen parçalar ve kopuk döküntüler
  onarılır; temiz bir model değişmeden geçer.
- **Girdiden asla kötü değil.** Her onarım adımı, yalnızca sonucu onarımın o
  ana kadarki halinden en az onun kadar iyiyse benimsenir; değilse atılır.
- **Dürüst sonuç.** Sonuç su geçirmez denmeden önce yeniden yüklenip tekrar
  kontrol edilir; rapor neyin düzeltildiğini, neyin kaldığını ve şeklin ne
  kadar değiştiğini gösterir.
- **Çok araç, tek düğme.** On dokuz sıralı onarım yöntemi tek bir *Onar*
  düğmesinin arkasındadır; triage motoru modelin neye ihtiyaç duyduğunu seçer.
- **Yerel ve çevrimdışı.** Geometriyi Rust çekirdeği (`sutura_geom`) yapar;
  kurulumdan sonra internet bağlantısı gerekmez.
- **İş akışına uyar.** KDE'de Dolphin servis menüsü, macOS'ta Finder Quick
  Action, deneysel bir OrcaSlicer eklentisi, toplu onarım ve çok nesneli 3MF.

## Kurulum

**Linux** — tek satır (güncel `main`'i kurar; yeniden çalıştırmak günceller):

```sh
curl -fsSL https://raw.githubusercontent.com/Krateian/Sutura/main/install.sh | bash
```

Kendi içinde tam bir **AppImage** her
[sürüme](https://github.com/Krateian/Sutura/releases/latest) eklenir:

```sh
chmod +x Sutura-x86_64.AppImage && ./Sutura-x86_64.AppImage
```

**macOS** — `Sutura-vX.Y.Z.dmg` dosyasını
[son sürümden](https://github.com/Krateian/Sutura/releases/latest) indirin
(imzasızdır: ilk açılışta sağ tık → *Aç*), ya da kaynaktan kurun:

```sh
git clone https://github.com/Krateian/Sutura.git && cd Sutura && ./install-macos.sh
```

Gereksinimler, fTetWild katmanı, güncelleme ve kaldırma
[Kurulum ve gereksinimler](docs/guide/tr/install.md) sayfasında anlatılır.

## Kullanım

```sh
sutura model.stl                 # model_fixed.stl yazar
sutura model.3mf -o fixed.3mf    # çıktı adını seçer
sutura model.stl --human         # nelerin düzeltildiğine dair okunur rapor
sutura validate model.stl        # yalnızca analiz, hiçbir şeyi değiştirmez
sutura a.stl b.stl c.3mf         # toplu onarım
```

- **GUI:** *Sutura*'yı uygulama menüsünden ya da Spotlight'tan açın, dosyaları
  ekleyin (veya sürükleyip bırakın) ve **Onar**'a basın.
- **KDE Dolphin:** modele sağ tık → **Repair with Sutura**.
- **macOS Finder:** sağ tık → *Hızlı Eylemler* → **Sutura — Repair**.
- **Son çare katı motoru (deneysel):** `--solid-engine cdt|ftetwild` her şey
  başarısız olduğunda kullanılacak yeniden inşayı seçer (Seçenekler → Onarım
  altında da; entegrasyonlarca devralınır). Varsayılan **fTetWild** olarak
  kalır; `cdt` Coaptation sonrası ve fTetWild öncesi çalışan deneysel bir kesin
  yeniden inşadır ve başarısız olursa ya da zaman aşımına uğrarsa fTetWild'a
  geçer. Şimdilik büyük mesh'lerde yavaştır; küçük parçalarda tercih edin.

Tüm seçenekler, rapor alanları ve entegrasyonlar [Kullanım](docs/guide/tr/usage.md)
sayfasındadır.

## Nasıl çalışır

Bozuk bir model hasarlı bir deri gibi ele alınır: onarım yaraya odaklanır,
sağlam deriye dokunmaz. Kısaca:

1. **Delik kapatma**, delikleri içeriden tıkar (*Tamponade*) ya da çevredeki
   yüzeyi takip ederek kapatır (*Flap*).
2. **Temizlik (Aşama 1)**, yüzeyin bağlantılarını düzeltir — duplike ve
   dejenere üçgenler, yanlış birleşmiş kenarlar, ters yüzler — ve kalan
   delikleri kapatır.
3. **Daha derin adımlar yalnızca gerektiğinde çalışır:** *Graft* sadece hasarlı
   bölgeyi yeniden kurar, *Coaptation* katlanmış ya da üst üste binmiş gövdeleri
   kaynaklayıp birleştirir, yedek katmanlar (*Splint*, *Dressing*,
   fTetWild) en zor durumları üstlenir.
4. **Aşama 2** tek, kapalı ve basılabilir bir katı üretir; sonuç raporlanmadan
   önce yeniden yüklenip kontrol edilir.

Ayrıntılı anlatım [Sutura nasıl çalışır](docs/guide/tr/how-it-works.md) sayfasındadır.

## Belgeler

| Konu | Türkçe | English |
|---|---|---|
| Sutura adım adım nasıl çalışır | [Nasıl çalışır](docs/guide/tr/how-it-works.md) | [How it works](docs/guide/how-it-works.md) |
| Onarım yöntemleri, yoğunluk presetleri ve harici motorlar | [Yöntemler](docs/guide/tr/methods.md) | [Methods](docs/guide/methods.md) |
| Kurulum, gereksinimler, güncelleme ve kaldırma | [Kurulum](docs/guide/tr/install.md) | [Install](docs/guide/install.md) |
| CLI, GUI, raporlar, Dolphin, Quick Action, OrcaSlicer | [Kullanım](docs/guide/tr/usage.md) | [Usage](docs/guide/usage.md) |
| Sık karşılaşılan sorunlar ve çözümleri | [Sorun giderme](docs/guide/tr/troubleshooting.md) | [Troubleshooting](docs/guide/troubleshooting.md) |
| Her alanın olgunluk durumu | [Özellik durumu](docs/guide/tr/feature-status.md) | [Feature status](docs/guide/feature-status.md) |
| Sağlamlık ve bilinen sınırlamalar | [Sınırlamalar](docs/guide/tr/limitations.md) | [Limitations](docs/guide/limitations.md) |
| Mesh türü tespiti (mekanik / organik) | [Sınıflandırıcı](docs/guide/tr/mesh-classifier.md) | [Mesh classifier](docs/guide/mesh-classifier.md) |
| Mimari ve kütüphaneler | [Mimari](docs/guide/tr/architecture.md) | [Architecture](docs/guide/architecture.md) |
| Anonim kullanım geçmişi (kapatılabilir) | [Gizlilik](docs/guide/tr/privacy.md) | [Privacy](docs/guide/privacy.md) |
| Testler ve benchmark (geliştiriciler) | [Test](docs/guide/tr/testing.md) | [Testing](docs/guide/testing.md) |
| Özellik sürümleri tek bakışta | [Sürüm geçmişi](docs/guide/tr/version-history.md) | [Version history](docs/guide/version-history.md) |
| Tüm değişiklikler ayrıntılı | — | [CHANGELOG.md](CHANGELOG.md) |

## Geliştiriciden bir not

Sutura, havacılık bakımında tam zamanlı bir işin yanında tek bir geliştirici
tarafından sürdürülür. Sürümler sabit bir takvime bağlı değildir: çalışmalar
sürekli olarak `main`'e eklenir ve bir grup iş hazır olduğunda sürüm çıkarılır;
bu yüzden düzenli bir sürüm temposu beklenmemelidir. Proje aktif olarak
geliştirilmektedir; Linux tek satırlık kurulum her zaman `main`'i takip eder.

## Katkı

Hata bildirimleri, anlaşılmayan belgeler, onarılamayan mesh'ler ve pull
request'lerin hepsi memnuniyetle karşılanır — küçük katkılar büyük özellikler
kadar değerlidir. Bir mesh bildirirken lütfen `sutura <dosya> --human` çıktısını
(ya da JSON raporunu), çalıştırılan komutu ve biliniyorsa mesh'in nasıl
üretildiğini (dilimleyici, tarayıcı, CAD dışa aktarımı) ekleyin. Hata şablonu
`.github/ISSUE_TEMPLATE/bug_report.md` dosyasındadır; ayrıca
[CONTRIBUTING.md](CONTRIBUTING.md) dosyasına bakın.

## Destekçiler

Sutura zaman ya da bir baskı kazandırdıysa,
[geliştirme GitHub üzerinden desteklenebilir](https://github.com/sponsors/Krateian).

## Lisans

v0.2.0'dan beri Sutura [PolyForm Noncommercial 1.0.0](LICENSE) lisansıyla
dağıtılır. Kişisel, ticari olmayan kullanım — hobi, araştırma, eğitim, kişisel
3D baskı — ücretsizdir ve öyle kalacaktır; bu kalıcı bir taahhüttür. Ticari
kullanım ayrı bir anlaşma gerektirir:
[Ticari Lisans Talep formunu](https://docs.google.com/forms/d/e/1FAIpQLSdqFrO11fjIBACrFwovOKGPpKVdnPcwxW0VRaQfwCqGayfJig/viewform)
doldurun (yanıtlar gizlidir). v0.1.0–v0.1.9 sürümleri kalıcı olarak
[Apache 2.0](https://github.com/Krateian/Sutura/blob/v0.1.9/LICENSE) lisansıyla
kalır.

Değişikliğin nedeni: Sutura kişisel bir ihtiyaçtan doğdu ve geliştirilmesinde
kullanılan yapay zekâ araçlarının maliyeti geliştirici tarafından karşılanıyor.
Ücretsiz ticari kullanım projeye karşı adil olmaz. Geliştirici bir gün devam
edemeyecek duruma gelirse, plan Sutura'yı tamamen açık kaynak yapıp topluluğa
devretmektir.
