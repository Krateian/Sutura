# Mimari ve kütüphaneler

[← README'ye dön](../../../README.tr.md) · [English](../architecture.md)

## Kütüphaneler

| Kütüphane | Rol | Neden |
|---|---|---|
| Yerel Aşama 1 motoru (`sutura_engine` + Rust `sutura_geom`) | aşama 1 filtre zinciri | VCG `meshing_*` eşdeğeri temizlik (yinelenen/dejenere temizliği, non-manifold onarımı, tutarlı yönlendirme, ear-clipping delik kapatma) yerel olarak uygulanmış; harici C++ çalışma zamanı yok, izin verici lisanslı |
| Manifold (`sutura_geom` içindeki `manifold-csg`) | aşama 2 yeniden kurma, dış kabuk birleşimi, yinelenen öge CSG, Graft yerel dikişi | su geçirmezlik garantisi, sağlam boolean, Bambu Studio ile aynı C++ çekirdek; statik bağlantı, Apache-2.0 |
| numpy | hat geneli | CLI, GUI, kusur tespiti, geçmiş ve tüm saf-Python katmanlar için dizi matematiği; zorunlu `sutura_geom` Rust eklentisi isteğe bağlı değildir ve Flap yolunun numpy geri dönüşü yoktur |
| pytetwild | fTetWild yedek katmanı (varsayılan olarak kurulur) | fTetWild tetrahedralizasyonu (MPL-2.0); yaklaşık 6 MB, PyVista/VTK kullanılmaz, bu yüzden varsayılan olarak gelir ve `SUTURA_WITH_FTETWILD=0` ile atlanır |
| sutura-geom (Rust: `robust`, `manifold-csg`, `num-bigint`, `num-rational`, `num-traits`, `rayon`, `faer`, `kiddo`, `pyo3`, `numpy`; gömülü Geogram Delaunay PSM, BSD-3-Clause) | exact arrangement, Manifold CSG, morfoloji, en yakın komşu, Flap, Dressing, Tamponad ve topoloji-temizleme hızlandırma | `--experimental-autorefine` ve `--experimental-indirect-autorefine` için exact açık/dolaylı predikatlar (`orient3d`, `insphere`), Aşama 2, dış kabuk birleşimi, yinelenen öge CSG ve Graft yerel dikişi için statik bağlı C++ Manifold çekirdeği (`manifold-csg`), aralık filtresi, exact üçgen kesişimi ve CDT, Graft (#13) için SDF ızgarası ve ikili konturlama, `flap_fill` yüzey-delik doldurma, `dressing_coat` dar-bant çıkarıcısı, `tamponade` iç-tıkaç modülü, nokta-bulutu en yakın komşuları (`kiddo`), exact EDT/dilasyon ve Aşama-1 temizliğini şeffaf bir saf-numpy geri dönüşle destekleyen yerel ağ-temizleme filtreleri (`remove_duplicate_faces`, `remove_null_faces`, mükerrer/kullanılmayan köşe kaldırma, köşe kaynatma) ve `--solid-engine cdt` arkasındaki `cdt3d` kısıtlı-dörtyüzlüleme katı yeniden inşası (isteğe bağlı, deneysel); `faer` (MIT) Flap fairing'i için seyrek LU sağlar. Yalnızca serbest lisanslar (MIT/Apache-2.0/BSD; gömülü Geogram Delaunay PSM zorunludur ve her `sutura_geom` derlemesine girer — hazır wheel'lerde bulunur, kaynak derlemeleri C++20 gerektirir; bildirimi ve LICENSE dosyası wheel içinde gelir). `scripts/install_sutura_geom.sh` ile otomatik tekerlek kurulumu veya `maturin develop` ile derleme |

`requirements.txt`, `requirements-gui.txt` ve
`requirements-ftetwild.txt` içinde sabitlenmiştir (geliştirme/test
bağımlılıkları `requirements-dev.txt` içinde); Rust bağımlılıkları
`rust/sutura-geom/Cargo.toml` içindedir (kaynaktan derleme Rust >= 1.89,
`kiddo` 6.3 MSRV gerektirir). SciPy de bir bağımlılık değildir: en yakın komşu,
EDT ve dilasyon Rust çekirdeğinde yaşar. `trimesh` artık bir çalışma-zamanı
bağımlılığı değildir: mesh G/Ç ve saf-Python geometri yardımcıları motorun
içinde yer alır ve test takımı / geliştirici betikleri `trimesh`'i yalnızca
geliştiriciye özel `requirements-dev.txt` üzerinden kurar.

## Mimari

Sutura, güvenilirlik, katman izolasyonu ve genişletilebilirlik için tasarlanmış modüler bir motor paketi (`sutura_engine`) etrafında yapılandırılmıştır:

* **`sutura_engine.core`** — Genel geometri ve doğrulama: mesh dizi G/Ç (STL, OBJ, 3MF), topolojik ölçümler ve Hausdorff mesafe kontrolleri; özelleşmiş geçişleri yeniden dışa aktarır.
* **`sutura_engine.diagnosis`** — Yüzey ve kusur analizleri: parça analizi, genişletilebilir geometrik şablonlar (mekanik, organik, rölyefler, ince yüzeyler, kafesler) ve mesh sınıflandırıcı entegrasyonu.
* **`sutura_engine.chart`** — `~/.cache/sutura/` altında 500 MB LRU boyut sınırına sahip SHA-256 içerik adresli önbellek; sürüm güncellemelerinde otomatik geçersiz kılma, CLI (`--no-cache`, `sutura clear-cache`) ve GUI seçenekleri.
* **`sutura_engine.hull`** — Çok bileşenli montajlar için dış yüzey kabuğu çıkarımı.
* **`sutura_engine.cast`** — Rust `sutura_geom` uzantısı etrafında ince sarmalayıcı (kesin yüklemler, `arrangement_lite`).
* **`sutura_engine.methods`** — `RepairMethodProtocol` standardına uyan eklenebilir onarım yöntemleri: yöntem başına bir adlandırılmış modülden otomatik olarak keşfedilen yerleşik yöntemler (`#1` Hızlı Temizlik - `#19` Koaptasyon), tarama kapatma, vekil şablon, morfoloji graft ve tekrarlayan geometri katmanları; #18 Splint isteğe bağlı tam katı yeniden inşa katmanıdır, #19 Koaptasyon ise varsayılan açık dikiş kaynağı birleştirme katmanıdır.
* **`sutura_engine.triage`** — Akıllı kademelendirme politikası: geometrik analize göre onarım yöntemlerini puanlar, kullanıcı yoğunluk profillerini (*Hızlı*, *Dengeli*, *Kapsamlı*, *Ekstrem*) uygular ve çalışma süresi bütçelerini yönetir.
* **`sutura_engine.adapters`** — Harici ve üçüncü parti kütüphaneler için katı izolasyon sınırları (fTetWild ve harici CLI motorları, artı Manifold kullanılabilirlik yoklaması). Manifold bir Python adaptörü değildir: C++ çekirdeği, `manifold-csg` aracılığıyla zorunlu `sutura_geom` Rust çekirdeğine statik olarak bağlanır. Yalnızca ilgili yöntem çalıştırıldığında tembel (lazy) olarak yüklenir.

Kurulu veya AppImage düzeninde `sutura_engine` üst düzey bir pakettir; kaynak kopyasında ise kök dizindeki küçük bir başlatma paketi (`sutura_engine/__init__.py`) sembolik bağ olmadan `import sutura_engine`'in depo kökünden çalışmasını sağlar.

### Sutura'ya özgü yaklaşımlar

* **X-Ray yeniden yükleme dürüstlüğü (`P-HONEST`).** Bellek içi topolojik sayımlara güvenmek yerine, Sutura su geçirmezlik kararını yalnızca diske yazılan float32 mesh'in yeniden okunduğunda kesin olarak kapalı olması durumunda verir (`defects.detect()` 0 delik ve 0 manifold-olmayan kenar bildirir). Yeniden yükleme sırasında float duyarlılığı nedeniyle kenar ayrılmaları oluşursa, Stitch geçişi (`P-WELD`) yüz topolojisini bozmadan teğet kenarları kaynaklar; bu kapalı döngü denetimi v0.5.1 derleminde 7 sahte-su geçirmez sonucu yakalamıştır.
* **Graft harmanlama hibriti (Yöntem #13).** Kesin Öklid mesafe dönüşümleri (Felzenszwalb & Huttenlocher 2012) ve ikili konturlama (Ju ve ark. 2002) ile hesaplanan matematiksel morfoloji kabuklarını (Nooruddin & Turk 2003; Suriyababu ve ark. 2023), hasarsız orijinal üçgenlerin birebir korunmasıyla birleştirir. Yeniden inşa edilen yama yalnızca hasarlı bölgeleri kaplar; sapma oluştuğunda fTetWild karşılaştırmasına başvuran tek taraflı sağlıklı-sadakat denetimi (`fidelity_ok`) ile sınırlanarak kabul edilen corpus modellerinde 0.0 sağlıklı sapma sağlar (bkz. `docs/repair-benchmark-strict-watertight-2026-09.md`). *Not: Graft (#13), `sutura_geom` Rust eklentisini gerektirir; bu eklenti kurulumcularla ve AppImage/.dmg paketleriyle birlikte gelir; eklenti zorunludur; eksik veya eski bir eklenti Graft/fTetWild geri dönüşü değil, adlandırılmış bir başlangıç hatasıdır.*
* **Transplant tekrar onarımı (Yöntem #11–#12).** Ön bölütleme gerektirmeksizin dönel, öteleme kafesi, helisel ve yansıma simetrilerindeki tekrarlayan geometrik ögeleri tespit eder. Tekrarlanan bir elemanın üzerinde bölgesel hasar saptandığında, sağlam bir eş kopyası hedefe hizalanıp aktarılarak boşluk kapatılır; işlem hedef bölgenin dışındaki Hausdorff kısıtıyla (`hausdorff_outside`) korunur.
* **Triage kademelendirme politikası (`sutura_engine.triage`).** Nesne başına geometrik özellik tanısını (bileşen yapısı, açık sınır oranları, simetri puanları) sıralı yöntem önerileri ve süre bütçeli geri dönüş mekanizmasıyla (`min(max(30 s, 2 × taban), 120 s)`) birleştirir. Kullanım geçmişi etkin olduğunda, sınırlı bir Bayes güncellemesi (Beta(2, 2) soncul ortalaması) yöntem seçim olasılıklarını ağ bağlantısı kullanmaksızın yerel olarak optimize eder.
* **Chart içerik adresli önbellek (`sutura_engine.chart`).** Ara ve nihai onarım verilerini SHA-256 geometri özetiyle `~/.cache/sutura/` altında 500 MB LRU tasfiye ilkesiyle önbelleğe alır. Tekrarlanan işlem adımlarını ve çok nesneli 3MF dosyalarının işlenmesini hızlandırır; sürüm değişimlerinde veya parametre güncellemelerinde girdileri otomatik olarak geçersiz kılar.

### Kritik yolları Rust çekirdeğine taşıma — tamamlandı

Bu yol haritasının anlattığı geçiş bitti. Rust çekirdeği artık bir Python
programının eklentisi değil, programın kendisidir:

- **Aşama 0 (Python İçi İyileştirmeler):** dış kabuk kontrolündeki gereksiz
  `trimesh` içe aktarımının kaldırılması ve dönel simetri tespitinde aday
  dönüşümlerin vektörize edilmesi.
- **Aşama 1 / 1c (Rust Topoloji Katmanı):** belirleyici dizi işlemleri yerel
  Rust'ta — X-Ray yeniden yükleme kaynaştırması, kesin delik/manifold-olmayan
  kenar tespiti, sınır döngüsü taraması ve Aşama 1 delik kapatmayı doğrudan
  diziler üzerinde yürüten ear-clipping kapatıcısı.
- **Aşama 2 (PyMeshLab Bağımsızlığı):** yinelenen/dejenere yüz temizliği,
  non-manifold onarımı, tutarlı yönlendirme, yinelenen/kullanılmayan köşe
  temizliği ve köşe kaynaştırma yereldir.
- **Aşama 3 (Yerel Manifold):** Manifold doğrudan Rust çekirdeğine bağlıdır;
  Aşama 2, dış kabuk birleşimi, yinelenen öge CSG ve Graft yerel dikişi orada
  çalışır.
- **Aşama 4 (Bağımsız Rust CLI) — bitti.** CLI ve GUI Rust ikilileridir. Çalışma
  zamanında Python süreci, sanal ortam veya yorumlayıcı yoktur; Python paketleri
  yalnızca önceki sürüm hâlâ desteklendiği sürece tutulur ve mevcut bir kurulum
  yeniden kurulmak yerine taşınır (bkz. [MIGRATE.md](../../../MIGRATE.tr.md)).

Yukarıdaki tabloda sayılan Aşama 1 filtre zinciri, Manifold, kesin predikatlar,
morfoloji, Flap, Dressing, Tamponad ve SDF/dual contouring çalışması bugün
`rust/sutura-geom` ve `rust/sutura-core` içindedir.
