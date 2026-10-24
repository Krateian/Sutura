# Sürüm geçmişi

[← README'ye dön](../../../README.tr.md) · [English](../version-history.md)


Yalnızca kullanıcıya yönelik özellik ekleyen sürümler listelenir (yalnızca
düzeltme içeren sürümler atlanır). Ayrıntılı bilgi [CHANGELOG.md](../../../CHANGELOG.md).

- **v0.9.5 — 2026-10-25** — **Diagnosis v2**, onarımın kendisini değiştirmeden
  ayrıntılı ve deterministik bir analiz raporu ekler (yönelim, parçalar,
  kendisiyle-kesişimler, sınır halkaları, ince duvarlar ve daha fazlası).
  Onarımlar artık tekrarlanabilir: Flap, Tamponad, Koaptasyon, Graft ve Splint
  sınırları ile Full Mend self-intersection kesme geçişi geçen süre yerine iş
  sayar; aynı dosya hızlı ya da yüklü makinede aynı sonucu verir (isteğe bağlı
  fTetWild eklentisi bu garanti dışındadır). Otomatik yedek yöntemler, tespit
  edilen hasarın anlamsız kıldığı yöntemleri atlar; Flap, Graft, Tamponad,
  eş düzlemli temas ve iç boşluklar daha güvenlidir. Belgeler İngilizce ve
  Türkçe konu sayfalarına taşındı.
- **v0.9.4 — 2026-10-23** — Varsayılan olarak açık bir dikiş kaynağı
  birleştirmesi olan **Koaptasyon (#19)**: parçaların temas ettiği dikiş
  boyunca katlanmış ya da üst üste binmiş çok gövdeli bir modeli birleştirir ve
  kapalı parçaları tek bir temiz katıya indirger; yalnızca o ana kadarki onarım
  sonucundan kesin olarak daha iyiyse benimsenir. Ayrıca düz bir katlanma
  dikişinin kendisiyle kesişen yüzlerle kapatılması, sağlıklı çok parçalı bir
  modelin parçalarını kaybetmesi, parçaların birleşiminin yüzeyi girdiden
  dışarı itmesi ve bir yüzeyi kapalı tutan yük taşıyan sıfır alanlı yüzlerin
  silinmesi düzeltildi; dosyadaki üçgen sırasına bağlı olan bazı sonuçlar artık
  her seferinde aynı ve kendini güncelleme bir sürümün gönderdiği her modülü
  kuruyor.
- **v0.9.3 — 2026-10-22** — **Artık PyMeshLab yok:** delik kapatma,
  self-intersection tespiti, temizlik, sadeleştirme (decimation), mesafe ölçümü
  ve mesh okuma/yazma tamamen Sutura'nın kendi Rust çekirdeğinde ve Python
  motorunda çalışıyor; kurulum daha küçük ve basit. 40 mesh'lik test
  setinde hiçbir mesh kötüleşmedi. Balloon (#8) artık native olarak ve
  self-intersection üretmeden yeniden inşa ediyor; 0.5.1–0.9.1 kurulumları
  yeniden kendini güncelleyebiliyor.
- **v0.9.2 — 2026-10-21** — İsteğe bağlı kesin katı yeniden inşa katmanı
  **Splint (#18)** (varsayılan kapalı; 40 mesh'lik korpusta değerlendirildi ve
  henüz hiçbirinde benimsenmedi); `sutura --self-check`, `--diagnostics` ve
  `--self-repair`; baskı-etkisi raporu; daha küçük çalışma zamanı
  (trimesh/SciPy kaldırıldı, Manifold ve yerel geometri zorunlu Rust
  çekirdeğinde); ve 3MF onarımlarında proje dosyasının korunması.
- **v0.9.0 — 2026-10-11** — **Tamponad** (#17 yöntemi, varsayılan açık iç delik
  tıkacı; içten şişirilen **Stent** adayı ve büyük halkalar için çok-stent
  bölümlemesi; katı tıkaç kabul kuralları — sızıntı yok, yeniden yüklemede yeni
  delik/non-manifold kenar yok, %20'yi aşan ters yönlü çift duvar yok, bağlı
  kalmalı ve parça eklememeli); **Flap** halka-başına aday seçimi (varsayılan
  açık, alternatif dolgular yalnızca birincil yama reddedilince denenir ve Flap
  bütçesinden düşülür; `SUTURA_FLAP_SELECT=0` kapatır); Scaffold resample (#10)
  için bellek/süre koruması; native exact self-intersection tespiti; ve
  bağımlılık azaltımı (Faz 1 `psutil` /
  `pyrobust-predicates` kaldırır, Faz 2a/2b Aşama-1 temizliğini Rust çekirdeğine
  yönlendirir).
- **v0.8.1 — 2026-10-03** — Onarımlar yerel bir Rust topoloji çekirdeğiyle
  hızlanır (köşe kaynatma, mükerrer yüz ve kenar sayımı; bayt bayt aynı çıktı,
  örnek modellerde uçtan uca %4–29) ve şeffaf bir saf-numpy yedek yolu bulunur;
  Aşama-2 katı yeniden inşasının dilimleyicide tek parça olarak açılmasını sağlayan
  otomatik birleşim-sonrası mikroskobik kıymık temizliği (gerçek çok parçalı
  montajlara dokunmaz); ve kusur raporu ile ısı haritasında düzeltilmiş manifold
  olmayan yüz indeksleri.
- **v0.8.0 — 2026-10-03** — **Flap** yüzey delik doldurma (bir sınır halkasını düz
  kapak yerine komşu yüzeyi sürdürerek kapatır; varsayılan açık, `--no-flap` /
  `SUTURA_FLAP=0` kapatır); **Pansuman** (#16 yöntemi, isteğe bağlı değişken
  viskoziteli seviye kümesi kaplaması) ile su geçirmez olmayan sonuçlarda öneri ve
  `--dressing-force-adopt`; `--si-mode` self-intersection politikası (varsayılan
  *Yalnızca raporla*); Graft girdi yüz sınırı + duvar-saati bütçesi; ve Tam
  Onarım'da korumalı Aşama-1 self-intersection eksizyonu + inceltilmiş yeniden
  kapama.
- **v0.7.2 — 2026-10-02** — OrcaSlicer eklentisi v2 (yan panel, onarım bildirimleri,
  doğru dönüşümler, geri yükleme); simetri tespiti 2-3,7 kat hızlı.
- **v0.7.1 — 2026-10-01** — Graft artık kurulu derlemelerde çalışıyor: Rust
  eklentisi önceden derlenmiş wheel'ler üzerinden sunuluyor ve AppImage/.dmg
  paketlerine dahil edildi; macOS .dmg başlangıç düzeltmesi.
- **v0.7.0 — 2026-09-28** — Sutura Triyaj Motoru: modüler mimari
  (`sutura_engine` altında Triyaj, Tanı/Diagnosis, Chart önbelleği, Dikiş/Stitch,
  X-Ray, Kabuk/Hull ve Döküm/Cast); genişletilebilir Eklenti API'si
  (`RepairMethodProtocol`); içerik adresli Chart önbelleği (`~/.cache/sutura/`);
  #13 **Graft** yöntemi (fTetWild öncesi sağlıklı yüzey sadakat korumalı morfoloji
  kabuk sarma); resmi görünen adlarla 16 yöntemli kayıt defteri; ve çalışma
  süresi bütçeli otomatik kademelendirme.
- **v0.6.1 — 2026-09-28** — İki isteğe bağlı geometri yöntemi: **Aynalı
  Tamamlama** (#14, tek-taraflı simetrik bir taramanın eksik arkasını algılanan
  simetri düzleminde görünen yüzeyi aynalayarak tamamlar; native örtük yedeğiyle) ve
  **Duvar Kalınlaştır** (#15, SDF ızgarasından tepe-başına duvar kalınlığı ve
  morfolojik hedefe-kalınlaştırma, `--wall-min-thickness T`); ayrıca **öğrenen
  triyaj** (küçük, sınırlı yerel `(nesne tipi, yöntem)` sıralama bonusu;
  `--no-learning-triage` / `learning_triage` ile açılıp kapanır,
  `sutura clear-learning` veya GUI Seçenekler ile sıfırlanır).
- **v0.6.0 — 2026-09-28** — Onarım yöntem kayıt defteri: on iki kararlı yöntem
  (`--list-methods`), nesne-başına analiz ve öneriler (`--analyze`), açık
  etiketleme (`--methods 2,3,5`) ve GUI sağ tık **dosya-başına yöntem
  etiketleme**; **tek-taraflı tarama kapatma** (#8/#9), **vekil-şablon onarımı**
  (#10) ve **yinelenen öge onarımı** (#11/#12, otomatik algılama + GUI
  kaynak/hedef seçici) için yeni bağımsız katmanlar; **yeniden-yükleme dürüstü
  su geçirmezlik kararı** (P-HONEST) ve **P-WELD** son geçişi sayesinde eskiden
  yeniden yüklemede non-manifold olan yedi gerçek dünya örneği artık yüz sayıları
  değişmeden katı-su geçirmez yeniden yüklenir; yeni çalışma-zamanı bağımlılıkları
  scipy ve trimesh.
- **v0.5.0 — 2026-09-26** — Triage Engine: **yoğunluk presetleri ve
  adlandırılmış profiller** (`--intensity quick|balanced|thorough|extreme|<profil>`,
  GUI açılır listesi + profil düzenleyicisi), bir **derin-onarım merdiveni**
  (`--deep-repair {off,local,full}`) ve fTetWild yoğun-çıktı sadeleştirmesi,
  **harici onarım motorları** (`~/.config/sutura/engines/*.toml`, `chain.toml`,
  her sonuç fTetWild ile aynı su geçirmezlik + Hausdorff korumasından geçer)
  ve bir **fTetWild kur/kaldır yöneticisi** (`sutura ftetwild`, GUI *Motorlar*
  sekmesi), **sekmeli Seçenekler penceresi**, açık `--ftetwild-optimize` ve
  CI Linux/macOS uçtan uca kurulumlar. fTetWild yedek katmanı eki kuruluysa
  varsayılan olarak açıktır (kapatmak için `--no-fallback-ftetwild`); exact
  Rust arrangement yeniden ~2,8× hızlandı (thingi10k_1038441: M2'de 29,8 sn →
  10,6 sn, çıktı birebir aynı).
- **v0.4.0 — 2026-09-25** — deneysel self-intersection katmanları: yinelemeli
  snap-rounding'li autorefine ve fTetWild yedek katmanı (isteğe bağlı ~1,1 GB
  ek), ikisi de CLI bayrağı + GUI kutusu olarak; exact Rust arrangement
  (`--experimental-indirect-autorefine`) artık yoğun taramaları tamamlıyor
  (aralık filtreli predikatlar, kesişimler orijinal veriden); OrcaSlicer
  eklentisi seçili modeli `orca.host` üzerinden bellekte onarıyor (hiç tag'lenmemiş
  0.3.1 içeriği dahil).
- **v0.3.0 — 2026-09-20** — kavisli-mekanik sınıflandırıcı sinyali
  (`developable_fraction`) + 40 mesh'lik gerçek dünya corpus'u; 115 mesh'lik
  strict-watertight benchmark + corpus release asset'i + Manifold
  çapraz-doğrulama; GUI renk-kodlu kusur görünümü + "ne değişti" onarım
  günlüğü; opt-in edge-tiebreak sınıflandırıcı kafası ve join-components
  seçenekleri (CLI + GUI kutuları); sentetik defekt enjeksiyon aracı; 3MF
  zip-bomb koruması.
- **v0.2.7 — 2026-09-19** — birim-uyuşmazlığı tespiti; onarım bütçeleri; çok
  nesneli 3MF için nesne başına Aşama 2.
- **v0.2.6 — 2026-09-19** — Onarım Sağlık/Risk skorlaması; macOS Finder Quick
  Action.
- **v0.2.5 — 2026-09-19** — onarım profilleri (`--profile` / GUI açılır
  listesi); macOS yerel başlatma (Spotlight); gerçek dünya kalibrasyon corpus'u.
- **v0.2.3 — 2026-09-19** — bağımsız macOS `.app` için bundle-aware yol
  çözümü; imzasız macOS `.dmg` paketleme.
- **v0.2.1 — 2026-08-30** — anonim kullanım geçmişi (opt-out); graphify bilgi
  grafiği entegrasyonu; katkı bölümü.
- **v0.2.0 — 2026-08-26** — interaktif 3D görüntüleyici (döndürme/yakınlaştırma
  + yüzey sapması); `.obj` desteği; otomatik güncelleme lisans sınırı; sponsor
  bölümü.
- **v0.1.9 — 2026-08-25** — Onarım Güven Skoru; GUI Analyze düğmesi + analiz
  bölmesi; mod önerileri.
- **v0.1.8 — 2026-08-24** — `validate` alt komutu; `--dry-run`; en-kötü-kusur
  yakınlaştırmalı öncesi/sonrası karşılaştırma.
- **v0.1.7 — 2026-08-22** — onarım modları (`--mode` + GUI seçici); extreme ek
  geçişler; öncesi/sonrası mesh karşılaştırması (GUI).
- **v0.1.6 — 2026-08-20** — raporlarda öncesi/sonrası geometri farkı; `--diff`.
- **v0.1.4 — 2026-08-19** — batch onarım özeti; kusur tespiti
  (`sutura/defects.py`); mesh-tipi farkında onarım; Türkçe README.
- **v0.1.3 — 2026-08-19** — opt-in güncelleme denetçisi; yedek ve geri
  dönüşlü kendi-kendine güncelleme.
