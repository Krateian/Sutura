# Mimari ve kütüphaneler

[← README'ye dön](../../../README.tr.md) · [English](../architecture.md)

## Kütüphaneler

| Kütüphane | Rol | Neden |
|---|---|---|
| Yerel Aşama 1 motoru (`sutura_engine` + Rust `sutura_geom`) | aşama 1 filtre zinciri | VCG `meshing_*` eşdeğeri temizlik (yinelenen/dejenere temizliği, non-manifold onarımı, tutarlı yönlendirme, ear-clipping delik kapatma) yerel olarak uygulanmış; harici C++ çalışma zamanı yok, izin verici lisanslı |
| Manifold (`sutura_geom` içindeki `manifold-csg`) | aşama 2 yeniden kurma, dış kabuk birleşimi, yinelenen öge CSG, Graft yerel dikişi | su geçirmezlik garantisi, sağlam boolean, Bambu Studio ile aynı C++ çekirdek; statik bağlantı, Apache-2.0 |
| numpy | hat geneli | CLI, GUI, kusur tespiti, geçmiş ve tüm saf-Python katmanlar için dizi matematiği; zorunlu `sutura_geom` Rust eklentisi isteğe bağlı değildir ve Flap yolunun numpy geri dönüşü yoktur |
| pytetwild + pyvista | fTetWild yedek katmanı (isteğe bağlı ek) | fTetWild tetrahedralizasyonu (MPL-2.0); pyvista VTK'yı getirir, kurulu ~1,1 GB, bu yüzden `SUTURA_WITH_FTETWILD=1` ile isteğe bağlıdır |
| sutura-geom (Rust: `robust`, `manifold-csg`, `num-bigint`, `num-rational`, `num-traits`, `rayon`, `faer`, `kiddo`, `pyo3`, `numpy`; gömülü Geogram Delaunay PSM, BSD-3-Clause) | exact arrangement, Manifold CSG, morfoloji, en yakın komşu, Flap, Dressing, Tamponad ve topoloji-temizleme hızlandırma | `--experimental-autorefine` ve `--experimental-indirect-autorefine` için exact açık/dolaylı predikatlar (`orient3d`, `insphere`), Aşama 2, dış kabuk birleşimi, yinelenen öge CSG ve Graft yerel dikişi için statik bağlı C++ Manifold çekirdeği (`manifold-csg`), aralık filtresi, exact üçgen kesişimi ve CDT, Graft (#13) için SDF ızgarası ve ikili konturlama, `flap_fill` yüzey-delik doldurma, `dressing_coat` dar-bant çıkarıcısı, `tamponade` iç-tıkaç modülü, nokta-bulutu en yakın komşuları (`kiddo`), exact EDT/dilasyon ve Aşama-1 temizliğini şeffaf bir saf-numpy geri dönüşle destekleyen yerel ağ-temizleme filtreleri (`remove_duplicate_faces`, `remove_null_faces`, mükerrer/kullanılmayan köşe kaldırma, köşe kaynatma); `faer` (MIT) Flap fairing'i için seyrek LU sağlar. Yalnızca serbest lisanslar (MIT/Apache-2.0/BSD; gömülü Geogram Delaunay PSM zorunludur ve her `sutura_geom` derlemesine girer — hazır wheel'lerde bulunur, kaynak derlemeleri C++20 gerektirir; bildirimi ve LICENSE dosyası wheel içinde gelir). `scripts/install_sutura_geom.sh` ile otomatik tekerlek kurulumu veya `maturin develop` ile derleme |

`requirements.txt`, `requirements-gui.txt` ve isteğe bağlı
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

### Yol haritası: kritik yolları Rust çekirdeğine taşıma

Gelecekteki geliştirme süreci, performans açısından kritik yolları `rust/sutura-geom` bileşenine taşıyarak Python çalışma zamanı yükünü azaltmayı hedeflemektedir:
- **Aşama 0 (Python İçi İyileştirmeler):** Dış kabuk kontrolündeki gereksiz `trimesh` içe aktarımının kaldırılması v0.7.0 ile sunulmuştur; dönel simetri tespitindeki aday dönüşümlerin vektörize edilmesi ise v0.7.2 ile sunulmuştur (toplu, iş parçacıklı en-yakın-komşu sorguları, birebir aynı sonuçlar, örnek/korpus detektöründe ~2-3,7 kat daha hızlı).
- **Aşama 1 (Rust Topoloji Katmanı):** Belirleyici dizi işlemlerinin (X-Ray yeniden yükleme kaynaştırması, kesin delik/manifold-olmayan kenar tespiti ve sınır döngüsü taraması) yerel Rust'a aktarılması ve en yakın komşu sorguları için yerel BVH yapılarının kullanılması.
- **Aşama 1c (Rust Delik Üçgenleme — tamamlandı):** yerel Rust `sutura_geom.close_holes` ear-clipping kapatıcısı (VCGLib `meshing_close_holes` semantiğiyle uyumlu) artık Aşama 1 delik kapatma adımını doğrudan NumPy dizileri üzerinde yürütür. Liepa tarzı minimum ağırlıklı üçgenleme ise gelecek çalışması olarak durur.
- **Aşama 2 (PyMeshLab Bağımsızlığı — tamamlandı):** yinelenen/dejenere yüz temizliği, non-manifold onarımı, tutarlı yönlendirme, yinelenen/kullanılmayan köşe temizliği ve köşe kaynaştırma, saf-numpy geri dönüşüyle yerel Rust (`sutura_geom`) içinde uygulanmıştır; PyMeshLab artık harici bir C++ çalışma zamanı bağımlılığı değildir.
- **Aşama 3 (Yerel Manifold — tamamlandı):** Manifold, `manifold-csg` aracılığıyla doğrudan Rust eklentisine bağlanmıştır; Aşama 2, dış kabuk birleşimi, yinelenen öge CSG ve Graft yerel dikişi `sutura_geom` içinde çalışır, Python `manifold3d` köprüleri kaldırılmıştır ve 3.11 sanal ortamı artık yalnızca isteğe bağlı fTetWild eki için vardır.
- **Aşama 4 (Bağımsız Rust CLI):** bağımsız bir Rust CLI çalıştırılabilir dosyasına doğru gelecekteki çalışma olarak kalır.
