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
  ekler; sağ tık → Quick Actions altında her seçili dosya için görünür, yalnızca
  STL/3MF dosyaları onarılır, diğer seçimler bildirimle atlanır. Quick Action,
  bir PyInstaller `Sutura.app` içindeki paketli `sutura-cli`'yi çağırır; bu yüzden yalnızca böyle bir uygulama mevcutsa
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
* **Yoğun eş düzlemli temaslar sınırlı bir exact geçişle analiz edilir.**
  Tek bir temas yamasında çok sayıda çakışık yüz bulunan mekanik bir parçada
  exact eş düzlemli kabuk analizi, temas kümesi başına 512 üçgen / 1.024
  kısıt segmenti, düzlem başına 1.024 üçgen ve toplamda 8.192 işlenmiş yüz ile
  sınırlandırılır. Bir sınıra ulaşıldığında exact geçiş atlanır, girdi
  değişmeden korunur ve normal Aşama 1 onarımı devam eder; rapor
  `tamponade_shell_prepass` altında `budget_exceeded` ve
  `outer_shell_skipped: shell_budget_exceeded` gösterir. Bu, çok yoğun
  temaslı bir mesin onarımı kilitlemesini önler; ancak böyle bir mesh
  watertight yerine `warning` olarak bitebilir.
* **Ters çevrilmiş tüm modeller.** Onarılan hacim negatif çıkarsa tüm mesh
  çevrilir; tutarlı şekilde "içi dışında" sarmalanmış bir model otomatik
  düzeltilir.
* **Manifold çekirdeği.** Açık kenarlı herhangi bir girdiyi reddeder;
  aşama 1 bir deliği kapatamazsa aşama 2 atlanır ve aşama 1 sonucu olduğu gibi
  kullanılır (rapor bunu belirtir).
* **Deneysel katı motoru (CDT).** İsteğe bağlı kesin `--solid-engine cdt`
  yeniden inşası şimdilik yavaştır: birçok modelde, birkaç bin yüzlü olanlarda
  bile zaman sınırı içinde bitmez ve Sutura o zaman otomatik olarak fTetWild'a
  döner. Küçük ya da basit modellerde veya orijinal yüzeyin birebir korunmasını
  istediğinizde kullanın. CDT'nin sonucu denetimlerini geçemezse (su geçirmez, tek kapalı ve
  dışa dönük katı) ya da zaman aşımına uğrarsa merdiven fTetWild'a geçer (o da
  kendi zaman sınırıyla çalışır, bu yüzden en kötü durum yaklaşık iki kat sürer);
  yani yine de bir sonuç üretilir ve rapor hangi motorun çalıştığını
  `solid_engine` altında kaydeder.
* **STL yeniden yükleme ve sıkışmış köşeler.** STL köşe paylaşımı saklamaz;
  bellekte iki-manifold olan bir mesh, iki ayrı float32 konumunun birleşmesiyle
  yeniden yüklendiğinde sıkışmış köşeler taşıyabilir. P-WELD yaygın float32
  çakışması durumunu ayırır; çözemezse rapor dürüst kalır
  (`reload_watertight=false`, `reload_non_manifold_vertices`). Patolojik
  durumlarda kaydedilen dosyayı dilimleyicinizde kontrol edin.
* **Kasıtlı iç boşluklar ve döküntü.** Aşama 2, gerçek bir iç boşluğu (bir CAD
  cebi veya mıknatıs, ağırlık ya da insert için açılmış bir delik) doldurmak
  yerine korur; ancak bu, birleşimin değiştireceği tek şeyin o boşluk olması ve
  boşluğu korumanın yeni kendisiyle-kesişim eklememesi ve boşluğun gövdeden
  belirgin biçimde küçük olması (sınırlayıcı kutu köşegeni gövdeninkinin
  0,5'inin altında) durumunda geçerlidir. Gövdeyi veya başka bir kabuğu kesen
  bir oyuk kabuğu reddedilir ve yerine olağan birleşim sonucu kullanılır;
  gövdenin paralel bir kopyası olan aşırı büyük bir iç kabuk da cep sayılmaz ve
  aynı şekilde doldurulur. Aşama 2'nin aynı zamanda döküntü de kaldırması
  gereken bir modelde yer alan bir boşluk ise şimdilik hâlâ doldurulur, çünkü
  mevcut düzeltme döküntüyü bırakıp boşluğu aynı anda koruyamaz. Boşluk
  korunduğunda rapor `cavity_preserved` der; `cavity_si_whole` /
  `cavity_si_union` / `cavity_rejected_si` kendisiyle-kesişim karşılaştırmasını,
  `cavity_rejected_bbox` / `cavity_bbox_ratio` ise boyut kontrolünü açıklar.
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
