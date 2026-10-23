# Sağlamlık ve bilinen sınırlamalar

[← README'ye dön](../../../README.tr.md) · [English](../limitations.md)

## Sağlamlık

Bozuk veya düşmanca girdiler, açık bir hata ve sıfır olmayan bir çıkış koduyla
reddedilir; asla çökme veya sessizce yanlış sonuç yoktur:

| Girdi | Davranış |
|---|---|
| Kesilmiş / yarıda kalmış ikili STL | reddedilir: "Unable to open file ... Malformed file" |
| Başlık, dosyanın tuttuğundan fazla üçgen iddia eder | reddedilir: "Malformed file" |
| NaN/Infinity köşe koordinatları | reddedilir: "input mesh contains NaN or infinite coordinates" |
| Boş mesh (0 üçgen) | reddedilir: "input mesh is empty (no triangles)" |
| Tamamen dejenere mesh (yalnızca sıfır alanlı yüzler) | reddedilir: "all faces are degenerate; nothing to repair" |
| Yanlış uzantı (`.stl` içinde OBJ içeriği veya tersi) | reddedilir: "Unable to open file" |

Bunlardan herhangi biri çıkış kodu 1 döndürür, böylece betikler hatayı
güvenilir şekilde algılayabilir.

## Bilinen sınırlamalar

* **macOS'ta sağ tık, Dolphin tarzı menü değil Quick Action gerektirir.**
  macOS'ta ServiceMenu karşılığı yoktur; kurulumcu bunun yerine
  `~/Library/Services/` içine bir Finder **Quick Action** ("Sutura — Repair")
  ekler ve seçili STL/3MF dosyaları için sağ tık → Quick Actions altında
  kullanılabilir. Quick Action, bir PyInstaller `Sutura.app` içindeki paketli
  `sutura-cli`'yi çağırır; bu yüzden yalnızca böyle bir uygulama mevcutsa
  (`/Applications` veya `~/Applications` içinde) onarım yapar — dev kurulum
  sarmalayıcı uygulaması tek başına paketli CLI içermez. İndirilmiş (karantinalı)
  bir `Sutura.app`, Quick Action onu çalıştırmadan önce bir kez sağ tık → Aç
  ile açılmalıdır (Gatekeeper; bildirim size bunu söyler).
* **macOS kaldırma betiği yok.** Linux `uninstall.sh`'inin macOS karşılığı
  yoktur; macOS kurulumunu kaldırmak elle yapılır (macOS kurulum bölümündeki
  "macOS kurulumunu kaldırma" bölümüne bakın).
* **Yerel KDE dosya diyaloğu.** GUI, QFileDialog'un yerel KDE diyaloğunu
  (lastik bant dikdörtgen seçimi dahil) kullanması için
  `QT_QPA_PLATFORMTHEME=kde` ayarlar ve `QT_PLUGIN_PATH`'i
  `/usr/lib/qt6/plugins`'e yönlendirir. Bu yalnızca sistem Qt sürümü
  paketlenmiş PySide6 Qt'siyle eşleştiğinde çalışır — GUI, sürümleri
  (`qmake` ile) kontrol eder ve sistem eklentilerini yalnızca eşleşmede
  kullanır. Farklı olduklarında (ör. sistem Qt 6.11.2'ye karşı paketli
  6.11.1), sistem platform eklentileri paketli Qt'ye yüklenemez, bu yüzden
  GUI, Qt'yi kendi paketli eklentilerinde tutar ve gömülü diyaloğa geri
  döner — dikdörtgen seçim kullanılamayabilir, ama Ctrl/Shift+tık her zaman
  çalışır.
* **Bağlı bir kabuk içinde kendisiyle kesişimler.** Manifold meshi bir katı
  olarak yeniden kurar, bu iç/örtüşen geometriyi çözer, ama patolojik
  durumlarda yeniden kurma, özellikleri hafifçe yeniden şekillendirebilir.
  Sonucu her zaman bir dilimleyicide kontrol edin.
* **Büyük delik yamaları.** Aşama 1 kapatıcısı, delikleri düz üçgen yamalarla doldurur; çok
  büyük delikler için dolgu basit bir yamadır, akıllı bir yeniden yapılandırma
  değildir. Meshi kapatır ama yama kalitesi ortalamadır ve yumuşatma
  gerektirebilir.
* **Küçük bağlantısız döküntüler.** 8 yüzden az olan bileşenler kaldırılır.
  Ana gövdeye bağlı olmayan küçük, meşru bir parça da kaldırılır.
* **Ters çevrilmiş tüm modeller.** Onarılan hacim negatif çıkarsa tüm mesh
  çevrilir; tutarlı şekilde "içi dışında" sarmalanmış bir model otomatik
  düzeltilir.
* **Manifold çekirdeği.** Açık kenarlı herhangi bir girdiyi reddeder;
  aşama 1 bir deliği kapatamazsa aşama 2 atlanır ve aşama 1 sonucu olduğu gibi
  kullanılır (rapor bunu belirtir).
* **STL yeniden yükleme ve sıkışmış köşeler.** STL köşe paylaşımı saklamaz;
  bellekte iki-manifold olan bir mesh, iki ayrı float32 konumunun birleşmesiyle
  yeniden yüklendiğinde sıkışmış köşeler taşıyabilir. P-WELD yaygın float32
  çakışması durumunu ayırır; çözemezse rapor dürüst kalır
  (`reload_watertight=false`, `reload_non_manifold_vertices`). Patolojik
  durumlarda kaydedilen dosyayı dilimleyicinizde kontrol edin.
* **Katmanlı/yinelenen köşeli 3MF dışa aktarımları.** Bazı dilimleyiciler
  (Bambu Studio dahil) nesnelerinin her köşe konumunu ~15 kez ayrı köşe girişi
  olarak tekrarlayan ve yüzeyleri katlanmış (bir kenarda birkaç yüz çakışık)
  olan 3MF'ler yazar. Aşama 1 zinciri bu tür meshleri geçerli 2-manifoldlara dönüştürebilir:
  Aşama 1 zinciri köşe tekilleştirmesinden SONRA yüzleri yeniden tekilleştirir
  (köşe tekilleştirmesi, katmanlı bir mesh üzerinde yinelenen yüzleri aslında
  *oluşturan* adımdır), sonra non-manifold köşeleri delik kapatmadan önce
  onarır ve sonda bir ek kapatma geçişi daha yapar; test dışa aktarımları artık
  tam kapalı onarılıyor — 0 kalan delik (boundary kenarlarının yarısı değil,
  gerçek boundary-loop sayısı olarak raporlanır). Kapalı katmanlı nesneler ayrıca
  nesne başına aşama 2 su geçirmez yeniden kurması alır, bu yüzden
  `stage2_skipped` değil su geçirmez raporlanırlar. Geliştirme sırasındaki bir
  örnek: daha önce 13 ve 26 mikro delik bildiren (veya yinelenen-yüz
  düzeltmesinden önce bazı platformlarda 0 yüze indirilen) 2 nesneli bir Bambu
  dışa aktarımı artık 0 ve 0 bildiriyor, ikisi de aşama 2 doğrulamalı.
* **Tüm nesneler ve proje verileri korunur.** 3MF'ler nesne nesne onarılır ve
  yalnızca onarılan `<mesh>` XML'i geri yazılır; diğer tüm arşiv girdileri
  (küçük resim, dilimleyici/plaka/wipe-tower ayarları, OPC ilişkileri ve
  Production alt modelleri) nesne/build kimlikleri, dönüşümler, birimler ve ad
  alanlarıyla birlikte korunur. Birden fazla mesh aynı `.model` girdisini
  paylaşsa bile hiçbir nesne kaybolmaz. Nesne başına sonuç (her nesnenin aşama 2
  kararı ve `triangle_attributes` tutulan/bırakılan sayıları dahil) CLI
  çıktısında ve GUI'de raporlanır.
