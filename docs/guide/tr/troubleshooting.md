# Sorun giderme

[← README'ye dön](../../../README.tr.md) · [English](../troubleshooting.md)


* **Kaldırma `sutura-env`'i korudu.** Sutura kurulumcusu oluşturmadıysa bu
  bilinçlidir. Kaldırıcı tam yolu, boyutu ve elle kaldırma komutunu yazdırır.
  Sahip olunan ortam için `./uninstall.sh --remove-env` (veya `--purge`)
  kullanın; `SUTURA_UNINSTALL_SKIP_CONDA=1` her zaman korur. `conda` `PATH`
  dışındaysa kaldırıcı için `SUTURA_CONDA_BIN=/conda/yolu` ayarlayın.
* **`python3.11` bulunamadı (yalnızca fTetWild yedek çalışma zamanı).**
  Aralık içinde bir `python3` ile normal kurulum Python 3.11 gerektirmez. Ana
  yorumlayıcı 3.10–3.14 aralığının dışındaysa `install.sh`, fTetWild katmanı
  için bir yedek çalışma zamanına ihtiyaç duyar ve
  `~/.local/share/sutura/python311` dizinine bağımsız bir CPython 3.11 indirir
  (`SUTURA_NO_PYTHON_DOWNLOAD=1` ile devre dışı bırakılabilir) veya sistem
  paketini kullanır:
  * Debian / Ubuntu 22.04+: `sudo apt install python3.11 python3.11-venv`
  * Fedora: `sudo dnf install python3.11`
  Hiç çalışma zamanı yoksa katmanı tamamen atlamak için `SUTURA_WITH_FTETWILD=0` verin.
* **`sutura_geom` kaynak derlemesi başarısız.** Manifold çekirdeği C++ ile
  derlenir: `cmake`, C++20 derleyicisi (g++/clang++) ve Rust >= 1.89 kurun.
  Sürüm kurulumları normalde hazır wheel kullanır.
* **PySide6 kurulumu başarısız.** GUI, `venv`'e PyPI'dan kurulan
  `PySide6-Essentials` gerektirir. Sürüm yorumlayıcıya göre seçilir:
  Python 3.12+ için `6.12.0`, Python 3.11 için `6.11.2`.
  `pip install PySide6-Essentials`'ın
  başarısız olduğu dağıtımlarda (eksik derleme araçları veya engellenen PyPI),
  sistem Qt Python bağlamalarını kurun ve giriş noktasını onlara yönlendirin:
  * Debian/Ubuntu: `sudo apt install python3-pyside6`
  * Fedora: `sudo dnf install python3-pyside6`
  * Arch: `sudo pacman -S pyside6` (resmi `extra` deposunda)
* **GUI'de KDE dosya diyaloğu yok.** Yerel diyalog, `plasma-integration` ve
  PySide6'nın Qt'siyle eşleşen bir sistem Qt sürümü gerektirir. Lastik bant
  seçimi yoksa Qt, gömülü diyaloğuna geri döner — çoklu seçim için Ctrl/Shift+tık
  yine de çalışır.
* **Yükseltilmiş kurulumda PyMeshLab kalır.** Yerel Aşama 1 motorundan bu yana
  PyMeshLab çalışma zamanında ne kurulur ne de içe aktarılır (yalnızca
  `requirements-dev.txt` içindeki isteğe bağlı test eşliği oracle'ı olarak kalır).
  Daha eski bir sürümden yükseltilen bir kurulumda yetim bir paket olarak
  kalabilir; `pip uninstall pymeshlab` (veya
  `conda remove -n sutura-env pymeshlab`) ile kaldırın. Sutura onu kullanmaz.
* **Headless GUI "açılışta takılıyor".** İlk çalıştırmada (config yok,
  `~/.config/sutura/config.json`) **modal** güncelleme/kullanım-geçmişi
  diyaloğu açılır ve yanıtlanana kadar engeller. 0.4.1'den sonraki sürümden
  itibaren bu diyalog Qt'nin pencere sistemi olmayan platformlarında
  (`QT_QPA_PLATFORM=offscreen` veya `minimal`) atlanır ve hiçbir şey
  yazılmaz; böylece ilk gerçek (ekranlı) açılışta yine sorulur. Kullanıcısı
  olmayan başka bir ekranda (ör. bir betikte yönlendirilmiş X oturumu)
  config'i önceden oluşturun, ör. `{"check_for_updates": false,
  "history_enabled": true}`. Güncelleme sorusu AppImage build'lerinde
  atlanır; geçmiş sorusu ilk çalıştırmada her zaman sorulur.
* **macOS: her `git push`'ta `grep: empty (sub)expression`.** Pre-push
  güvenlik hook'unun secret deseni, BSD grep'in reddettiği boş bir alternation
  dalı içeriyordu; secret taraması sessizce hiç çalışmıyordu.
  `scripts/pre-push-security-check.sh`'te düzeltildi (opsiyonel grup) — hook'u
  `./install.sh` / `./install-macos.sh` ile yeniden kurun.
* **Standalone .app: stage 2 atlandı / düşük güven.** Aşama 2 bundle içindeki
  `sutura_geom` eklentisinde çalışır. Eklenti bundle'da yoksa Aşama 2
  atlanmış olarak raporlanır ve güven ~25 puan düşer; `install-macos.sh` ile
  yeniden kurun.
* **Antipodal sarım sayısı regresyonu / devre dışı bırakma (opt-out).** Sutura;
  Graft, Tamponad, Dressing ve SDF üretiminde varsayılan olarak hızlandırılmış
  antipodal genelleştirilmiş sarım sayısını kullanır. Olağandışı bir modelde
  istenmeyen bir davranış görülürse veya klasik sarım sayısı orakılı ile tam
  uyum gerekirse, ortamda `SUTURA_ANTIPODAL=0` tanımlayarak klasik sarım yoluna
  dönebilirsiniz (Python'daki `antipodal=False` parametresi yalnızca Graft'ın
  `shell_wrap` işlevine ulaşır, `dressing.py`'de ise bir anahtar bulunmaz).

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
* **`sutura --diagnostics out.zip`** sistem ve Python çalışma zamanı
  bilgilerini, `sutura_geom` uzantısının durumunu, on dokuz yöntemin tümünün
  kullanılabilirliğini, kullanıcı ayarlarını ve en son onarım kaydını içeren
  tek bir ZIP arşivi yazar. Kişisel yollar ve kullanıcı adları arşivden
  temizlenir ve arşive hiçbir dosya adı konmaz; böylece bir hata raporuna
  eklenebilir.
* **`sutura --self-repair`** kurulu uygulama dosyalarını, kurulu sürüme ait
  sürüm bildirimine (manifest) göre denetler ve eksik ya da değişmiş bir
  dosyayı onarır. Her zaman değiştirmeden önce sorar, değiştirdiği dosyaları
  yedekler ve doğrulama başarısız olursa geri alır; kullanıcı verilerine
  (geçmiş, öğrenme, ayarlar) asla dokunmaz. AppImage ve macOS `.app`
  kurulumları salt-okunurdur, bu yüzden yerinde onarım yerine sürümler
  sayfasına bir işaret yazdırırlar. `sutura --self-repair --yes` onay istemini
  atlar.
