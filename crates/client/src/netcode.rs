use std::{net::SocketAddr, sync::Arc, thread::sleep, time::Duration};

use crossbeam::channel::{Receiver, Sender};
use game::{ClientPacket, GameEvent, ServerPacket};
use log::{error, info, warn};
use quinn::{
    crypto::rustls::QuicClientConfig,
    rustls::{
        self,
        pki_types::{CertificateDer, ServerName, UnixTime},
    },
    ClientConfig, Endpoint,
};

pub struct ServerConnection {
    send: Sender<ServerPacket>,
    recv: Receiver<GameEvent>,
    server: SocketAddr,
}

impl ServerConnection {
    pub fn new(send: Sender<ServerPacket>, recv: Receiver<GameEvent>, server: SocketAddr) -> Self {
        Self { send, recv, server }
    }

    /// Connect to the server and handle sending / receiving.
    pub async fn connect_and_handle(&mut self) -> Result<(), std::io::Error> {
        let config = ClientConfig::new(Arc::new(
            QuicClientConfig::try_from(
                rustls::ClientConfig::builder()
                    .dangerous()
                    .with_custom_certificate_verifier(SkipServerVerification::new())
                    .with_no_client_auth(),
            )
            .unwrap(),
        ));
        let mut endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(config);
        info!("client endpoint at {:?}", endpoint.local_addr().unwrap());
        info!("connecting to server...");
        // Receive events over the wire and send them to the instance manager.
        let connection = endpoint
            .connect(self.server, "localhost")
            .unwrap()
            .await
            .unwrap();
        info!("[client] connected: addr={}", connection.remote_address());
        let sender = self.send.clone();
        let recv_conn = connection.clone();

        tokio::spawn(async move {
            while let Ok(mut recv) = recv_conn.accept_uni().await {
                let bytes = recv.read_to_end(usize::MAX).await.unwrap();
                let packet = bincode::deserialize_from(&bytes[..]);
                let packet: ServerPacket = match packet {
                    Ok(e) => e,
                    Err(e) => {
                        warn!("Failed deserializing packet {:?}", e);
                        continue;
                    }
                };
                sender.send(packet).unwrap()
            }
        });

        while let Ok(e) = self.recv.recv() {
            if let Some(close_reason) = connection.close_reason() {
                error!("Server connection closed: {:?}", close_reason);
                break;
            }
            connection.handshake_data().unwrap();
            let mut send = connection.open_uni().await.unwrap();
            let payload = &bincode::serialize(&ClientPacket::GameEvent(e)).unwrap()[..];
            send.write_all(payload).await.unwrap();
            send.finish().unwrap();
            send.stopped().await.unwrap();
        }
        Ok(())
    }
}

/// Dummy certificate verifier that treats any certificate as valid.
/// NOTE, such verification is vulnerable to MITM attacks, but convenient for testing.
#[derive(Debug)]
struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self(Arc::new(rustls::crypto::ring::default_provider())))
    }
}

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
