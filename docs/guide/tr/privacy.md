# Kullanım geçmişi (gizlilik)

[← README'ye dön](../../../README.tr.md) · [English](../privacy.md)

## Kullanım geçmişi (anonim, isteğe bağlı kapatılabilir)

Sutura, onarımların *teknik kullanım geçmişini* kaydeder; böylece topluluk,
motoru (mesh_classifier + aşama 1/2 ayarları) gerçekte karşılaştığı vakalara
göre yönlendirebilir. Paylaşılan her kullanım geçmişi motoru zamanla daha
akıllı hale getiriyor — daha fazla gerçek dünya mesh'i, daha iyi classifier
ayarı ve daha az kaçırılan uç durum demek.

* **Ne kaydedilir:** mesh boyutu (köşe/yüz sayıları), kusur sayıları (delik,
  non-manifold, self-intersection), sınıflandırıcı sonucu (tür + güven),
  onarım modu, uygulanan filtre sayısı, nihai kategori, onarım-güven skoru,
  onarım süresi ve yalnızca geometriden türetilen bir parmak izi (tekrar
  eden mesh'leri ayıklamak için).
* **ASLA kaydedilmez:** dosya adları, dosya yolları, kullanıcı adları, IP
  adresleri, makine kimlikleri, ortam değişkenleri veya herhangi bir kişisel
  veri. Parmak izi yalnızca mesh geometrisinden türetilir.
* **Varsayılan:** açık. CLI'da `sutura --no-history ...` ile kapatın veya
  GUI'de ilk-çalıştırma penceresindeki kutuyu işaretlemeyin. Tercih
  `~/.config/sutura/config.json` içinde saklanır (`history_enabled`).
* **Paylaşım:** `sutura export-history` anonim bir özeti ve tam JSON
  kayıtlarını tek komutla basar — bir GitHub issue'suna yapıştırmaya hazır.
  `--summary-only` ile yalnızca özet, `--last N` ile son N kayıt,
  `--clear` ile dosyayı temizleyebilirsiniz. Hiçbir şey otomatik yüklenmez.
* **Nerede tutulur:** veri dosyaları (`history.jsonl`,
  `triage_learning.json`) uygulama kodundan ayrı olarak
  `~/.local/share/sutura/data/` altında saklanır; başka bir yerde tutmak için
  `SUTURA_DATA_DIR` ayarlayın (paketli kod bağlaması salt-okunur olan
  AppImage için kullanışlıdır). Kaldırıcı bu dizini varsayılan olarak korur;
  `uninstall.sh --purge` ise siler.
