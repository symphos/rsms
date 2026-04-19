//! Pipeline Layer trait - 装饰器模式

use crate::service::PduService;
use tower::Service;

/// 便捷函数: 用闭包创建 Layer
pub fn layer_fn<F, S>(f: F) -> LayerFn<F>
where
    F: Fn(S) -> <S as Service<PduRequest>>::Service,
    S: Service<PduRequest>,
{
    LayerFn(f)
}

pub struct LayerFn<F>(F);

impl<F, S> Service<PduRequest> for LayerFn<F>
where
    F: Fn(S) -> <S as Service<PduRequest>>::Service,
    S: Service<PduRequest>,
{
    type Response = <S as Service<PduRequest>>::Response;
    type Error = <S as Service<PduRequest>>::Error;
    type Future = <S as Service<PduRequest>>::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        // 这个不会直接使用，只是为了满足 Service trait
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: PduRequest) -> Self::Future {
        // 这里需要实际调用，但 LayerFn 只是用于类型定义
        // 实际使用时不直接调用这个
        panic!("LayerFn should not be called directly")
    }
}

/// 工具: 创建一个什么也不做的 Layer
#[derive(Clone)]
pub struct IdentityLayer;

impl<S> Service<PduRequest> for IdentityLayer
where
    S: Service<PduRequest> + Clone,
{
    type Response = <S as Service<PduRequest>>::Response;
    type Error = <S as Service<PduRequest>>::Error;
    type Future = <S as Service<PduRequest>>::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: PduRequest) -> Self::Future {
        panic!("IdentityLayer should not be called directly")
    }
}
